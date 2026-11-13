//! The decoder, from recorded and hand-written streams.
//!
//! Every fixture whose response is a stream is decoded here whole, cut in
//! two, one byte at a time, and in pieces of several fixed sizes. The
//! events must be the same each time. What they are is in a snapshot beside
//! the tests, one for each fixture, and the cases the build plan names are
//! also checked by hand below, with the differences between servers that a
//! fixture cannot show written out as streams in the tests themselves.
//!
//! **The work is bounded**, as it is for the other protocol
//! (`anthropic/decode.rs` says why). These streams are larger: one records
//! 104 kB for 385 tokens, because the server sends a chunk of 300 bytes
//! for each token of reasoning. So the number of places a stream is cut in
//! two goes down as the stream gets longer, and each stream costs about
//! the same.
//!
//! - **On every pull request,** each stream is delivered one byte at a
//!   time, which puts a boundary at every byte of it (of its first
//!   [`EXHAUSTIVE`] bytes, for a longer one), and in pieces of two other
//!   sizes. It is also cut in two at between 8 and 100 places
//!   spread over it, and at every one of its first [`EVERY_BYTE`] bytes
//!   when it is short.
//! - **In the nightly run,** each stream is delivered in pieces of seven
//!   other sizes, and each of up to [`EXHAUSTIVE`] bytes is cut in two at
//!   every byte, and a longer one at about 4,000 places.

use cortex_base::ErrorKind;
use cortex_llm::error::Throttle;
use cortex_llm::openai_chat::{flag, providers};
use cortex_llm::request::ReasoningVisibility;
use cortex_llm::{BlockKind, FlagValue, ModelEvent, ModelRequest, ProviderInfo};
use cortex_protocol::speed::SpeedFallbackBy;
use cortex_protocol::{Speed, StopReason};
use serde_json::{Value, json};

use super::scenario;
use super::{
    DEEPSEEK, OPENAI_LUNA, OPENROUTER, Server, VENICE, decode, decode_events, decode_whole,
    fixture_names, load, local_model, provider, provider_of,
};
use crate::anthropic::assert_snapshot;
use crate::anthropic::decode::render;

/// A request to a local model: what a user's own entry for one comes to.
fn local(provider: &str, id: &str, extra: &str) -> ModelRequest {
    ModelRequest::new(
        local_model(provider, id, extra),
        cortex_protocol::model::ModelRole::Main,
    )
}

/// A request to OpenAI's own model, at a speed.
fn openai(speed: Speed) -> ModelRequest {
    let mut request = scenario::base(providers::OPENAI, OPENAI_LUNA, Some("none"));
    request.speed = speed;
    request
}

/// The request a fixture's response answers, as far as the decoder cares:
/// the model, the speed it was sent at, and the flags.
pub(crate) fn request_of(name: &str) -> ModelRequest {
    let server = |server: &Server| scenario::desk(server, Vec::new());
    match name {
        "openrouter-fast" => scenario::tier(Speed::Fast),
        "openrouter-standard-named" => scenario::tier(Speed::Standard),
        "venice-fast" | "venice-error-fast-overloaded" => scenario::fast_model(&VENICE),
        "deepseek-models-2" => {
            scenario::desk_on(providers::DEEPSEEK, scenario::MODELS[1], "low", Vec::new())
        }
        "openai-fast" | "openai-fast-downgraded" => openai(Speed::Fast),
        "lmstudio-think-tags" => local(
            providers::LMSTUDIO,
            "qwen/qwen3-8b",
            r#"{"flags": {"reasoning_tags": true}}"#,
        ),
        _ if name.starts_with("openrouter-claude") => scenario::claude(Vec::new()),
        _ if name.starts_with("deepseek-") => server(&DEEPSEEK),
        _ if name.starts_with("openrouter-") => server(&OPENROUTER),
        _ if name.starts_with("venice-") => server(&VENICE),
        _ if name.starts_with("openai-") => openai(Speed::Standard),
        _ if name.starts_with("ollama-") => local(providers::OLLAMA, "qwen3:8b", "{}"),
        _ if name.starts_with("llamacpp-") => {
            local(providers::LLAMACPP, "qwen3-8b-q4_k_m.gguf", "{}")
        }
        _ if name.starts_with("vllm-") => local(providers::VLLM, "Qwen/Qwen3-8B", "{}"),
        other => panic!("no request is known for the fixture `{other}`"),
    }
}

/// How far into a short stream it is cut in two at every single byte, on a
/// pull request.
const EVERY_BYTE: usize = 64;

/// The longest stream that is cut in two at every byte in the nightly run.
const EXHAUSTIVE: usize = 16_000;

/// Whether this is the nightly run, which multiplies the property cases
/// and has the time for many more cuts.
fn nightly() -> bool {
    std::env::var("CORTEX_PROPERTY_SCALE")
        .ok()
        .and_then(|scale| scale.trim().parse::<u64>().ok())
        .is_some_and(|scale| scale > 1)
}

/// Where a stream of `len` bytes is cut in two.
fn cuts_in_two(len: usize) -> Vec<usize> {
    let places = if nightly() {
        if len <= EXHAUSTIVE {
            return (1..len).collect();
        }
        4_000
    } else {
        // About a quarter of a megabyte of decoding for one stream.
        (250_000 / len.max(1)).clamp(8, 50)
    };
    // The step is made odd so that the places are not all at even offsets.
    let step = (len / places).max(1) | 1;
    let short = len <= 20_000;
    (1..len)
        .filter(|cut| (short && *cut <= EVERY_BYTE) || cut % step == len % step)
        .collect()
}

/// Decodes a fixture whole, cut in two, a byte at a time and in pieces of
/// fixed sizes, checks they all agree, and compares the events with the
/// snapshot.
fn every_cut(name: &str) {
    let fixture = load(name);
    assert_eq!(fixture.status, 200, "{name} is not a stream");
    let request = request_of(name);
    let provider = provider_of(name);
    let stream = &fixture.response;
    let whole = decode_whole(&request, &provider, stream);
    assert!(!whole.is_empty(), "{name}: nothing was decoded");
    assert_well_formed(&whole);

    for cut in cuts_in_two(stream.len()) {
        let cut_in_two = decode(&request, &provider, stream, &[cut]);
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
    // event: two of them on a pull request, seven in the nightly run.
    let sizes: &[usize] = if nightly() {
        &[1, 2, 3, 5, 7, 13, 64, 500]
    } else {
        &[1, 7, 64]
    };
    for &size in sizes {
        // A long stream is delivered a byte at a time for its first
        // sixteen thousand bytes on a pull request, and in small pieces
        // after that: the parser looks at what it holds after every byte.
        let long = |at: &usize| !nightly() && size == 1 && *at > EXHAUSTIVE;
        let pieces: Vec<usize> = (1..stream.len())
            .filter(|at| {
                if long(at) {
                    at % 7 == 0
                } else {
                    at % size == 0
                }
            })
            .collect();
        assert!(
            decode(&request, &provider, stream, &pieces).as_ref() == Ok(&whole),
            "{name}: in pieces of {size} bytes"
        );
    }
    assert_snapshot(&format!("openai-chat/decode/{name}.txt"), &render(&whole));
}

/// Every stream fixture.
pub(crate) const STREAMS: &[&str] = &[
    "deepseek-text",
    "deepseek-reasoning",
    "deepseek-tool-call",
    "deepseek-tool-calls",
    "deepseek-max-tokens",
    "deepseek-conversation-1",
    "deepseek-conversation-2",
    "deepseek-conversation-3",
    "deepseek-conversation-4",
    "deepseek-switch-1",
    "deepseek-switch-2",
    "deepseek-insufficient-resource",
    "deepseek-tool-call-cut",
    "deepseek-models-1",
    "deepseek-models-2",
    "deepseek-models-3",
    "deepseek-stop-aborted",
    "openrouter-text",
    "openrouter-reasoning",
    "openrouter-tool-call",
    "openrouter-tool-calls",
    "openrouter-max-tokens",
    "openrouter-conversation-1",
    "openrouter-conversation-2",
    "openrouter-conversation-3",
    "openrouter-conversation-4",
    "openrouter-switch-1",
    "openrouter-switch-2",
    "openrouter-fast",
    "openrouter-standard-named",
    "openrouter-claude-1",
    "openrouter-claude-2",
    "openrouter-claude-3",
    "openrouter-error-after-200",
    "openrouter-tool-call-cut",
    "venice-text",
    "venice-reasoning",
    "venice-tool-call",
    "venice-tool-calls",
    "venice-max-tokens",
    "venice-conversation-1",
    "venice-conversation-2",
    "venice-conversation-3",
    "venice-conversation-4",
    "venice-switch-1",
    "venice-switch-2",
    "venice-fast",
    "venice-error-after-200",
    "venice-tool-call-cut",
    "openai-text",
    "openai-tool-calls-interleaved",
    "openai-refusal",
    "openai-content-filter",
    "openai-fast",
    "openai-fast-downgraded",
    "openai-unknown-event",
    "ollama-reasoning",
    "ollama-tool-call",
    "llamacpp-reasoning",
    "vllm-reasoning",
    "lmstudio-think-tags",
];

/// One test for each server, so that they run side by side. A failure
/// names its fixture. (One test for each stream would start a process for
/// each, which costs more than the decoding does.)
macro_rules! every_cut {
    ($($test:ident => $server:literal,)+) => {
        mod at_every_cut {
            $(
                #[test]
                fn $test() {
                    let prefix = concat!($server, "-");
                    let streams: Vec<&str> = super::STREAMS
                        .iter()
                        .copied()
                        .filter(|name| name.starts_with(prefix))
                        .collect();
                    assert!(!streams.is_empty(), "no stream of {}", $server);
                    for name in streams {
                        super::every_cut(name);
                    }
                }
            )+
        }

        /// The servers the tests above cover.
        const SERVERS: &[&str] = &[$($server),+];
    };
}

every_cut! {
    deepseek => "deepseek",
    openrouter => "openrouter",
    venice => "venice",
    openai => "openai",
    ollama => "ollama",
    llamacpp => "llamacpp",
    vllm => "vllm",
    lmstudio => "lmstudio",
}

#[test]
fn every_stream_in_the_fixtures_directory_is_replayed() {
    let on_disk: Vec<String> = fixture_names()
        .into_iter()
        .filter(|name| load(name).status == 200)
        .collect();
    let mut covered: Vec<&str> = STREAMS.to_vec();
    covered.sort_unstable();
    assert_eq!(
        on_disk, covered,
        "a stream fixture with no test, or a test with no fixture"
    );
    // And each belongs to a server that has a test.
    for name in STREAMS {
        let server = name.split('-').next().unwrap();
        assert!(
            SERVERS.contains(&server),
            "{name}: no test replays {server}'s streams"
        );
    }
}

#[test]
fn the_fixtures_come_from_more_than_two_servers_and_say_how_they_were_made() {
    let mut recorded: Vec<&str> = Vec::new();
    let mut by_hand: Vec<&str> = Vec::new();
    let names = fixture_names();
    for name in &names {
        let note = load(name).note;
        let server = name.split('-').next().unwrap();
        // A fixture is one or the other, and its note says which.
        match (
            note.contains("Recorded on"),
            note.contains("**Written by hand**"),
        ) {
            (true, false) => recorded.push(server),
            (false, true) => {
                assert!(
                    note.contains("A recording from the real server can replace it"),
                    "{name}"
                );
                by_hand.push(server);
            }
            other => panic!("{name}: the note does not say how the fixture was made: {other:?}"),
        }
    }
    recorded.sort_unstable();
    recorded.dedup();
    assert_eq!(recorded, ["deepseek", "openrouter", "venice"]);
    // Nothing was recorded from a server that was not there to record
    // from: OpenAI and the local servers are written by hand throughout.
    for server in ["openai", "ollama", "llamacpp", "vllm", "lmstudio"] {
        assert!(by_hand.contains(&server), "{server} has no fixture");
        assert!(!recorded.contains(&server), "{server}");
    }
}

fn events_of(name: &str) -> Vec<ModelEvent> {
    decode_whole(&request_of(name), &provider_of(name), &load(name).response)
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

fn text(events: &[ModelEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn reasoning(events: &[ModelEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Each tool call of a response: its id, its name, and its arguments as
/// the fragments add up.
fn tool_calls(events: &[ModelEvent]) -> Vec<(String, String, String)> {
    let mut calls: Vec<(u32, String, String, String)> = Vec::new();
    for event in events {
        match event {
            ModelEvent::ToolCallStart { index, id, name } => {
                calls.push((*index, id.clone(), name.clone(), String::new()));
            }
            ModelEvent::ToolArgsDelta { index, json } => {
                let call = calls
                    .iter_mut()
                    .find(|call| call.0 == *index)
                    .expect("arguments for a call that was announced");
                call.3.push_str(json);
            }
            _ => {}
        }
    }
    calls
        .into_iter()
        .map(|(_, id, name, arguments)| (id, name, arguments))
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

fn error_of(events: &[ModelEvent]) -> &cortex_llm::ProviderError {
    match events.last() {
        Some(ModelEvent::Error(error)) => error,
        other => panic!("the last event is not an error: {other:?}"),
    }
}

fn fallbacks(events: &[ModelEvent]) -> Vec<&cortex_llm::SpeedMove> {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::SpeedFallback(moved) => Some(moved),
            _ => None,
        })
        .collect()
}

/// What every response must look like: one start first, each block started
/// once and ended once, nothing added to a block that is not open, and
/// exactly one terminal event, last.
fn assert_well_formed(events: &[ModelEvent]) {
    let starts = events
        .iter()
        .filter(|event| matches!(event, ModelEvent::Start { .. }))
        .count();
    assert_eq!(starts, 1, "{events:?}");
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
            ModelEvent::BlockEnd { index, raw } => {
                assert!(open.contains(index), "{index} ended before it started");
                open.retain(|other| other != index);
                closed.push(*index);
                let block: Value = serde_json::from_str(raw.as_str()).expect("a block is JSON");
                assert!(block.is_object(), "{raw}");
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

/// A chunk with one choice, as the data of an event.
fn chunk(delta: &Value, finish: Option<&str>) -> String {
    json!({
        "id": "chatcmpl-1", "model": "m",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    })
    .to_string()
}

/// The request and provider the streams written in these tests are decoded
/// for: a model with no tier and no flag, at a server with no flag.
fn plain() -> (ModelRequest, ProviderInfo) {
    (
        local(providers::LLAMACPP, "a-model", "{}"),
        provider(providers::LLAMACPP),
    )
}

fn decode_plain(events: &[&str]) -> Vec<ModelEvent> {
    let (request, provider) = plain();
    decode_events(&request, &provider, events).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn text_is_its_deltas_and_the_piece_of_the_message_it_stands_for() {
    for name in [
        "deepseek-text",
        "openrouter-text",
        "venice-text",
        "openai-text",
    ] {
        let events = events_of(name);
        // OpenRouter's model always reasons, so its answer has a reasoning
        // block first.
        assert_eq!(kinds(&events).last(), Some(&BlockKind::Text), "{name}");
        let said = text(&events);
        assert!(said.to_lowercase().contains("hello"), "{name}: {said}");
        let blocks = raw_blocks(&events);
        assert_eq!(
            blocks.last().unwrap(),
            &json!({"content": said}),
            "{name}: the stored block is the message's `content`"
        );
        let (stop, raw_stop, _, usage) = end_of(&events);
        assert_eq!((stop, raw_stop), (&StopReason::End, "stop"), "{name}");
        assert!(usage.tokens.output > 0, "{name}");
        assert_eq!(usage.speed, Speed::Standard, "{name}");
        assert!(fallbacks(&events).is_empty(), "{name}");
    }
}

#[test]
fn reasoning_is_read_under_each_servers_name_for_it() {
    // The fixture, the member the server puts reasoning in, and what the
    // reasoning holds: the recorded models reason in their own words.
    let rows = [
        ("deepseek-reasoning", "reasoning_content", ""),
        ("venice-reasoning", "reasoning_content", ""),
        ("openrouter-reasoning", "reasoning", "17"),
        ("llamacpp-reasoning", "reasoning_content", "17 plus 23"),
        ("ollama-reasoning", "reasoning", "17 plus 23"),
        ("vllm-reasoning", "reasoning", "17 plus 23"),
    ];
    for (name, member, holds) in rows {
        let events = events_of(name);
        assert_eq!(
            kinds(&events),
            [BlockKind::Reasoning, BlockKind::Text],
            "{name}"
        );
        let thought = reasoning(&events);
        assert!(
            !thought.is_empty() && thought.contains(holds),
            "{name}: {thought}"
        );
        let blocks = raw_blocks(&events);
        // The stored block has the reasoning under the server's own name
        // for it, which is the name it goes back under.
        assert_eq!(blocks[0][member], json!(thought), "{name}: {}", blocks[0]);
        assert_eq!(blocks[1], json!({"content": text(&events)}), "{name}");
        // The answer is not in the reasoning, nor the reasoning in the
        // answer.
        assert!(!text(&events).is_empty(), "{name}");
        assert_eq!(end_of(&events).0, &StopReason::End, "{name}");
    }
}

#[test]
fn reasoning_sent_under_two_names_at_once_is_read_once() {
    // A server that sends the same reasoning as `reasoning_content` and as
    // `reasoning`, and one that sends it as `reasoning` and in pieces.
    let twice = chunk(&json!({"reasoning_content": "hm", "reasoning": "hm"}), None);
    let pieces = chunk(
        &json!({"reasoning": " ok", "reasoning_details": [
            {"type": "reasoning.text", "text": " ok", "index": 0}]}),
        None,
    );
    let events = decode_plain(&[
        &twice,
        &pieces,
        &chunk(&json!({"content": "done"}), Some("stop")),
        "[DONE]",
    ]);
    assert_eq!(reasoning(&events), "hm ok");
    assert_eq!(kinds(&events), [BlockKind::Reasoning, BlockKind::Text]);
    let blocks = raw_blocks(&events);
    // One run of reasoning is stored under the name it began under.
    assert_eq!(blocks[0]["reasoning_content"], "hm ok");
    assert_eq!(blocks[0]["reasoning_details"][0]["text"], " ok");
}

#[test]
fn a_provider_can_name_the_member_its_reasoning_is_in() {
    let stream = [
        chunk(&json!({"thinking": "from the named member"}), None),
        chunk(&json!({"reasoning_content": "from the usual member"}), None),
        chunk(&json!({"content": "done"}), Some("stop")),
        "[DONE]".to_owned(),
    ];
    let stream: Vec<&str> = stream.iter().map(String::as_str).collect();
    let (request, mut named) = plain();
    // Unset, the usual members are read and no other.
    let events = decode_events(&request, &named, &stream).unwrap();
    assert_eq!(reasoning(&events), "from the usual member");
    // Named, that member is read and no other.
    named.flags.set(
        flag::REASONING_FIELD,
        FlagValue::Text("thinking".to_owned()),
    );
    let events = decode_events(&request, &named, &stream).unwrap();
    assert_eq!(reasoning(&events), "from the named member");
    assert_eq!(
        raw_blocks(&events)[0],
        json!({"thinking": "from the named member"})
    );
}

#[test]
fn reasoning_inside_the_text_is_taken_out_where_the_flag_says_so() {
    let events = events_of("lmstudio-think-tags");
    assert_eq!(kinds(&events), [BlockKind::Reasoning, BlockKind::Text]);
    assert_eq!(reasoning(&events), "\n17 plus 23 is 40. A quarter is 10.\n");
    assert_eq!(text(&events), "\n\n30 sheep are left.");
    // The two stored blocks are together the content as it arrived.
    let blocks = raw_blocks(&events);
    assert_eq!(
        blocks[0],
        json!({"content": "<think>\n17 plus 23 is 40. A quarter is 10.\n</think>"})
    );
    assert_eq!(blocks[1], json!({"content": "\n\n30 sheep are left."}));

    // Without the flag the tags are text like any other: nothing is
    // guessed from what the text looks like.
    let unflagged = local(providers::LMSTUDIO, "qwen/qwen3-8b", "{}");
    let events = decode_whole(
        &unflagged,
        &provider(providers::LMSTUDIO),
        &load("lmstudio-think-tags").response,
    );
    assert_eq!(kinds(&events), [BlockKind::Text]);
    assert!(text(&events).starts_with("<think>"));
    assert_eq!(reasoning(&events), "");
}

#[test]
fn reasoning_that_is_not_to_be_shown_is_still_stored() {
    for name in ["deepseek-reasoning", "openrouter-claude-1"] {
        let mut request = request_of(name);
        let shown = events_of(name);
        request.reasoning.visibility = ReasoningVisibility::None;
        let hidden = decode_whole(&request, &provider_of(name), &load(name).response);
        assert!(!reasoning(&shown).is_empty(), "{name}");
        assert_eq!(reasoning(&hidden), "", "{name}");
        // The blocks, and what is stored for each, are the same: a server
        // that wants its reasoning back still gets it.
        assert_eq!(kinds(&hidden), kinds(&shown), "{name}");
        assert_eq!(raw_blocks(&hidden), raw_blocks(&shown), "{name}");
        assert_eq!(text(&hidden), text(&shown), "{name}");
    }
}

#[test]
fn signed_reasoning_arrives_in_pieces_and_is_stored_whole() {
    let events = events_of("openrouter-claude-1");
    assert_eq!(kinds(&events)[0], BlockKind::Reasoning);
    assert!(
        kinds(&events)[1..]
            .iter()
            .all(|kind| *kind == BlockKind::ToolCall)
    );
    let blocks = raw_blocks(&events);
    let block = &blocks[0];
    // The visible text is the server's `reasoning`, and the pieces are
    // joined into one, with the signature that came in a piece of its own.
    assert_eq!(block["reasoning"], json!(reasoning(&events)));
    let details = block["reasoning_details"].as_array().unwrap();
    assert_eq!(details.len(), 1, "{block}");
    assert_eq!(details[0]["type"], "reasoning.text");
    assert_eq!(details[0]["text"], block["reasoning"]);
    assert_eq!(details[0]["format"], "anthropic-claude-v1");
    assert_eq!(details[0]["index"], 0);
    assert!(
        details[0]["signature"]
            .as_str()
            .is_some_and(|signature| !signature.is_empty()),
        "{block}"
    );
}

#[test]
fn a_tool_call_is_announced_with_its_id_and_name_and_stored_as_the_message_has_it() {
    for name in [
        "deepseek-tool-call",
        "openrouter-tool-call",
        "venice-tool-call",
        "ollama-tool-call",
    ] {
        let events = events_of(name);
        let calls = tool_calls(&events);
        assert_eq!(calls.len(), 1, "{name}");
        let (id, tool, arguments) = &calls[0];
        assert_eq!(tool, "get_weather", "{name}");
        assert_eq!(
            serde_json::from_str::<Value>(arguments).unwrap(),
            json!({"city": "Paris"}),
            "{name}"
        );
        let block = raw_blocks(&events).pop().unwrap();
        assert_eq!(
            block,
            json!({"tool_calls": [{
                "id": id, "type": "function",
                "function": {"name": "get_weather", "arguments": arguments},
            }]}),
            "{name}: the stored block is an item of the message's `tool_calls`"
        );
        let (stop, raw_stop, _, _) = end_of(&events);
        assert_eq!(
            (stop, raw_stop),
            (&StopReason::ToolCalls, "tool_calls"),
            "{name}"
        );
    }
}

#[test]
fn several_tool_calls_are_several_blocks_each_with_its_own_arguments() {
    for name in [
        "deepseek-tool-calls",
        "openrouter-tool-calls",
        "venice-tool-calls",
    ] {
        let events = events_of(name);
        let calls = tool_calls(&events);
        let cities: Vec<Value> = calls
            .iter()
            .map(|(_, _, arguments)| {
                serde_json::from_str::<Value>(arguments).unwrap()["city"].clone()
            })
            .collect();
        assert_eq!(cities, ["Paris", "Tokyo", "Lima"], "{name}");
        let mut ids: Vec<&str> = calls.iter().map(|(id, _, _)| id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), 3, "{name}: each call has an id of its own");
        assert_eq!(end_of(&events).0, &StopReason::ToolCalls, "{name}");
    }
}

#[test]
fn tool_call_fragments_belong_to_the_call_their_index_names() {
    // The fragments of the two calls arrive interleaved, and the second
    // call's first: joined in the order they arrive, Oslo's arguments
    // would be written into Cairo's call.
    let events = events_of("openai-tool-calls-interleaved");
    assert_eq!(
        tool_calls(&events),
        [
            (
                "call_fixture9001".to_owned(),
                "get_weather".to_owned(),
                r#"{"city":"Oslo"}"#.to_owned()
            ),
            (
                "call_fixture9002".to_owned(),
                "get_weather".to_owned(),
                r#"{"city":"Cairo"}"#.to_owned()
            ),
        ]
    );
    let blocks = raw_blocks(&events);
    assert_eq!(
        blocks[0]["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"Oslo"}"#
    );
    assert_eq!(
        blocks[1]["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"Cairo"}"#
    );
    // Both calls stay open until the stop reason: neither is ended by the
    // other's beginning.
    let ends: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| matches!(event, ModelEvent::BlockEnd { .. }).then_some(at))
        .collect();
    let last_fragment = events
        .iter()
        .rposition(|event| matches!(event, ModelEvent::ToolArgsDelta { .. }))
        .unwrap();
    assert!(ends.iter().all(|end| *end > last_fragment), "{ends:?}");
}

#[test]
fn a_call_without_an_id_gets_one_and_a_name_that_comes_late_is_waited_for() {
    // No id at all: a result must still be able to name the call.
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"name": "read", "arguments": "{}"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 3, "function": {"name": "list", "arguments": "{}"}}]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    // The id is made from the response's id and the call's place in the
    // response, so that no other call of the conversation has it.
    assert_eq!(
        tool_calls(&events),
        [
            (
                "call_chatcmpl1_0".to_owned(),
                "read".to_owned(),
                "{}".to_owned()
            ),
            (
                "call_chatcmpl1_1".to_owned(),
                "list".to_owned(),
                "{}".to_owned()
            ),
        ]
    );
    assert_eq!(
        raw_blocks(&events)[1]["tool_calls"][0]["id"],
        "call_chatcmpl1_1"
    );
    // A response with no id of its own: the call's place alone.
    let nameless = json!({"choices": [{"index": 0, "finish_reason": "tool_calls", "delta":
        {"tool_calls": [{"index": 0, "function": {"name": "read", "arguments": "{}"}}]}}]})
    .to_string();
    assert_eq!(
        tool_calls(&decode_plain(&[&nameless, "[DONE]"]))[0].0,
        "call_0"
    );
    // Two such calls in one response are still two ids.
    let two = json!({"choices": [{"index": 0, "finish_reason": "tool_calls", "delta":
    {"tool_calls": [
        {"index": 0, "function": {"name": "read", "arguments": "{}"}},
        {"index": 1, "function": {"name": "read", "arguments": "{}"}},
    ]}}]})
    .to_string();
    let ids: Vec<String> = tool_calls(&decode_plain(&[&two, "[DONE]"]))
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(ids, ["call_0", "call_1"]);

    // The arguments begin before the name is known: the call is announced
    // when it is, with everything that arrived before.
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"arguments": "{\"pa"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"name": "read", "arguments": "th\":"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"arguments": "1}"}}]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(
        tool_calls(&events),
        [(
            "call_a".to_owned(),
            "read".to_owned(),
            "{\"path\":1}".to_owned()
        )]
    );

    // No index: each call is whole in its delta, and its place in the
    // list is its index.
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [
                {"id": "a", "function": {"name": "read", "arguments": "{\"n\":1}"}},
                {"id": "b", "function": {"name": "read", "arguments": "{\"n\":2}"}},
            ]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    let calls = tool_calls(&events);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        ("b".to_owned(), "read".to_owned(), "{\"n\":2}".to_owned())
    );
}

#[test]
fn what_a_call_carries_that_the_decoder_does_not_read_goes_back_with_it() {
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{
                "index": 0, "id": "call_a", "type": "function",
                "function": {"name": "read", "arguments": "{}"},
                "extra_content": {"vendor": {"thought_signature": "c2ln"}},
            }]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    assert_eq!(
        raw_blocks(&events)[0],
        json!({"tool_calls": [{
            "id": "call_a", "type": "function",
            "function": {"name": "read", "arguments": "{}"},
            "extra_content": {"vendor": {"thought_signature": "c2ln"}},
        }]})
    );
}

#[test]
fn a_usage_object_replaces_the_one_before_it_and_is_never_added_up() {
    // OpenRouter repeats the stop reason with the usage, and its usage is
    // the request's whole count once.
    for name in [
        "openrouter-text",
        "openrouter-tool-calls",
        "openrouter-fast",
    ] {
        let fixture = load(name);
        let last: Value = fixture
            .response
            .split(|byte| *byte == b'\n')
            .filter_map(|line| line.strip_prefix(b"data: "))
            .filter_map(|data| serde_json::from_slice::<Value>(data).ok())
            .rfind(|chunk| chunk["usage"].is_object())
            .unwrap();
        let events = events_of(name);
        let usage = end_of(&events).3;
        assert_eq!(
            usage.tokens.output,
            last["usage"]["completion_tokens"].as_u64().unwrap(),
            "{name}"
        );
        let ends = events
            .iter()
            .filter(|event| matches!(event, ModelEvent::End { .. }))
            .count();
        assert_eq!(ends, 1, "{name}: a repeated stop reason ends nothing twice");
    }

    // A server that sends the count so far in every chunk.
    let usage = |prompt: u64, completion: u64| json!({"prompt_tokens": prompt, "completion_tokens": completion});
    let with = |delta: Value, finish: Option<&str>, used: Value| {
        json!({"id": "c", "model": "m", "usage": used,
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        .to_string()
    };
    let events = decode_plain(&[
        &with(json!({"content": "a"}), None, usage(10, 1)),
        &with(json!({"content": "b"}), None, usage(10, 2)),
        &with(json!({"content": "c"}), Some("stop"), usage(10, 3)),
        // And the same count again, alone.
        &json!({"id": "c", "model": "m", "choices": [], "usage": usage(10, 3)}).to_string(),
        "[DONE]",
    ]);
    let used = &end_of(&events).3.tokens;
    assert_eq!(
        (used.input, used.output),
        (10, 3),
        "the last count, not the sum"
    );
    let reported = events
        .iter()
        .filter(|event| matches!(event, ModelEvent::Usage(_)))
        .count();
    assert_eq!(reported, 4, "each count is passed on as it arrives");

    // A server that sends none: nothing is made up.
    let events = decode_plain(&[&chunk(&json!({"content": "a"}), Some("stop")), "[DONE]"]);
    let used = end_of(&events).3;
    assert_eq!((used.tokens.input, used.tokens.output), (0, 0));
    assert_eq!(used.raw, None);
}

#[test]
fn cached_tokens_come_out_of_the_prompt_count_whatever_the_server_calls_them() {
    // OpenAI's form: 1,219 prompt tokens of which 1,024 were cached.
    let usage = end_of(&events_of("openai-text")).3.tokens.clone();
    assert_eq!(
        (usage.input, usage.cache_read, usage.output),
        (195, 1024, 7)
    );
    // DeepSeek's own names, recorded: the second request of a conversation
    // reads the first from the cache.
    let events = events_of("deepseek-conversation-2");
    let usage = &end_of(&events).3;
    let raw: Value = serde_json::from_str(usage.raw.as_ref().unwrap().as_str()).unwrap();
    assert!(usage.tokens.cache_read > 0, "{raw}");
    assert_eq!(usage.tokens.cache_read, raw["prompt_cache_hit_tokens"]);
    assert_eq!(usage.tokens.input, raw["prompt_cache_miss_tokens"]);
    assert_eq!(
        usage.tokens.input + usage.tokens.cache_read,
        raw["prompt_tokens"].as_u64().unwrap()
    );
    // A model of Anthropic's through OpenRouter: the first request writes
    // the cache and the second reads it, and both are in the prompt count.
    let wrote = end_of(&events_of("openrouter-claude-1")).3.clone();
    let read = end_of(&events_of("openrouter-claude-2")).3.clone();
    assert!(wrote.tokens.cache_write > 0, "{:?}", wrote.tokens);
    assert_eq!(read.tokens.cache_read, wrote.tokens.cache_write);
    let raw: Value = serde_json::from_str(wrote.raw.as_ref().unwrap().as_str()).unwrap();
    assert_eq!(
        wrote.tokens.input + wrote.tokens.cache_read + wrote.tokens.cache_write,
        raw["prompt_tokens"].as_u64().unwrap()
    );
    // The reasoning count, where a server gives one.
    assert!(wrote.tokens.reasoning.is_some_and(|count| count > 0));
    assert!(wrote.tokens.reasoning.unwrap() <= wrote.tokens.output);
}

#[test]
fn a_cost_the_provider_states_is_read_from_where_it_puts_it() {
    // OpenRouter: in the usage. Venice.ai: beside it, in US dollars.
    for name in ["openrouter-text", "venice-text"] {
        let cost = end_of(&events_of(name)).3.provider_cost;
        assert!(
            cost.is_some_and(|nanos| (1..1_000_000).contains(&nanos)),
            "{name}: {cost:?}"
        );
    }
    let venice: Value = load("venice-text")
        .response
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"data: "))
        .filter_map(|data| serde_json::from_slice::<Value>(data).ok())
        .find(|chunk| chunk["cost"].is_object())
        .unwrap();
    let stated = (venice["cost"]["usd"].as_f64().unwrap() * 1e9).round();
    assert_eq!(
        end_of(&events_of("venice-text"))
            .3
            .provider_cost
            .map(|nanos| nanos as f64),
        Some(stated)
    );
    // DeepSeek states none.
    assert_eq!(end_of(&events_of("deepseek-text")).3.provider_cost, None);
}

#[test]
fn an_error_inside_a_successful_response_is_an_error_and_never_an_end() {
    // Recorded: Venice.ai began to answer and then failed.
    let events = events_of("venice-error-after-200");
    assert!(matches!(events.first(), Some(ModelEvent::Start { .. })));
    assert!(!reasoning(&events).is_empty(), "the response had begun");
    let error = error_of(&events);
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(error.code, "api_error");
    assert_eq!(error.status, Some(500), "the status the body carries");
    assert!(
        error.message.contains("object has no attribute"),
        "{error:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::End { .. })),
        "a failed response does not also end"
    );

    // OpenRouter's documented form: an `error` at the top of a chunk whose
    // choice stops with `error`.
    let events = events_of("openrouter-error-after-200");
    assert_eq!(text(&events), "The weather");
    let error = error_of(&events);
    assert_eq!(
        (error.kind, error.status),
        (ErrorKind::Transient, Some(502))
    );
    assert_eq!(error.message, "Provider disconnected unexpectedly");

    // DeepSeek's stop reason that is a failure.
    let events = events_of("deepseek-insufficient-resource");
    let error = error_of(&events);
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(error.code, "insufficient_system_resource");
    assert_eq!(error.throttle, Some(Throttle::Overload));

    // Other forms a server may give it.
    let rate_limited = decode_plain(&[
        &chunk(&json!({"content": "a"}), None),
        r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#,
        "[DONE]",
    ]);
    let error = error_of(&rate_limited);
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(error.throttle, Some(Throttle::RateLimit));
    assert_eq!(error.status, None);
    let stopped = decode_plain(&[&chunk(&json!({"content": ""}), Some("error")), "[DONE]"]);
    assert_eq!(error_of(&stopped).kind, ErrorKind::Transient);
    assert_eq!(error_of(&stopped).code, "error");
    // Nothing follows the error: not the `[DONE]`, and not a chunk a
    // server sends after it.
    for events in [&rate_limited, &stopped] {
        assert!(events.last().is_some_and(ModelEvent::is_terminal));
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    }
}

#[test]
fn an_event_named_error_fails_the_response_whatever_its_data_is() {
    let (request, provider) = plain();
    let stream = format!(
        "data: {}\n\nevent: error\ndata: upstream timed out\n\ndata: [DONE]\n\n",
        chunk(&json!({"content": "a"}), None)
    );
    let events = decode(&request, &provider, stream.as_bytes(), &[]).unwrap();
    let error = error_of(&events);
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(error.message, "upstream timed out");

    let stream = "event: error\ndata: {\"error\":\"Insufficient balance\",\"code\":\"INSUFFICIENT_BALANCE\"}\n\n";
    let events = decode(&request, &provider, stream.as_bytes(), &[]).unwrap();
    let error = error_of(&events);
    assert_eq!(
        error.kind,
        ErrorKind::Provider,
        "a named failure keeps its kind"
    );
    assert_eq!(error.code, "INSUFFICIENT_BALANCE");
}

#[test]
fn every_stop_reason_becomes_its_neutral_form() {
    let stopped = |name: &str| {
        let events = events_of(name);
        let (stop, raw, _, _) = end_of(&events);
        (stop.clone(), raw.to_owned())
    };
    // Recorded from all three servers: the output limit.
    for name in [
        "deepseek-max-tokens",
        "openrouter-max-tokens",
        "venice-max-tokens",
    ] {
        assert_eq!(
            stopped(name),
            (StopReason::MaxOutput, "length".to_owned()),
            "{name}"
        );
    }
    assert_eq!(
        stopped("openai-content-filter"),
        (StopReason::ContentFilter, "content_filter".to_owned())
    );
    // A refusal stops with `stop`; what makes it one is where its text
    // arrived.
    let refusal = events_of("openai-refusal");
    assert_eq!(end_of(&refusal).0, &StopReason::Refusal);
    assert_eq!(text(&refusal), "I can't help with that.");
    assert_eq!(
        raw_blocks(&refusal),
        [json!({"refusal": "I can't help with that."})]
    );

    // A stop reason nobody knows is passed on under its own name.
    let events = decode_plain(&[&chunk(&json!({"content": "a"}), Some("aborted")), "[DONE]"]);
    let (stop, raw, _, _) = end_of(&events);
    assert!(!stop.is_known(), "{stop:?}");
    assert_eq!((stop.as_str(), raw), ("openai:aborted", "aborted"));

    // The server's own word for the stop, where it gives one.
    let events = events_of("openrouter-fast");
    assert_eq!(end_of(&events).2, Some("completed"));
}

#[test]
fn a_response_with_tool_calls_stops_for_them_whatever_word_the_server_uses() {
    let call = json!({"tool_calls": [{"index": 0, "id": "a", "function": {"name": "read", "arguments": "{}"}}]});
    // Some servers end a response that calls tools with `stop`.
    let events = decode_plain(&[&chunk(&call, Some("stop")), "[DONE]"]);
    assert_eq!(end_of(&events).0, &StopReason::ToolCalls);
    assert_eq!(end_of(&events).1, "stop");
    // And a response that says `tool_calls` and holds none has nothing to
    // run.
    let events = decode_plain(&[
        &chunk(&json!({"content": "a"}), Some("tool_calls")),
        "[DONE]",
    ]);
    assert_eq!(end_of(&events).0, &StopReason::End);
    // The output limit is the output limit, calls or not.
    let events = decode_plain(&[&chunk(&call, Some("length")), "[DONE]"]);
    assert_eq!(end_of(&events).0, &StopReason::MaxOutput);
}

#[test]
fn the_tier_that_served_the_request_is_what_the_response_says() {
    // Recorded: asked for fast and served fast, asked for standard and
    // served standard.
    let events = events_of("openrouter-fast");
    let usage = end_of(&events).3;
    assert_eq!(
        (usage.speed.clone(), usage.speed_requested.clone()),
        (Speed::Fast, None)
    );
    assert!(fallbacks(&events).is_empty());
    let events = events_of("openrouter-standard-named");
    assert_eq!(end_of(&events).3.speed, Speed::Standard);
    assert!(fallbacks(&events).is_empty());
    // OpenAI's own form.
    let events = events_of("openai-fast");
    assert_eq!(end_of(&events).3.speed, Speed::Fast);
    assert!(fallbacks(&events).is_empty());

    // Asked for fast, served standard: recorded at standard, with what was
    // asked for beside it, and said once.
    let events = events_of("openai-fast-downgraded");
    let usage = end_of(&events).3;
    assert_eq!(usage.speed, Speed::Standard);
    assert_eq!(usage.speed_requested, Some(Speed::Fast));
    let moved = fallbacks(&events);
    assert_eq!(
        moved.len(),
        1,
        "said once, though every chunk names the tier"
    );
    assert_eq!(moved[0].by, SpeedFallbackBy::Provider);
    assert_eq!(
        (&moved[0].requested, &moved[0].served),
        (&Speed::Fast, &Speed::Standard)
    );
}

#[test]
fn a_tier_is_read_only_from_a_model_that_is_asked_for_one_that_way() {
    // Venice.ai's chunks say `priority`, which is its upstream host's
    // word, on a request it bills at its standard price. Its models are
    // not asked for a tier with `service_tier`, so the word is not read.
    for name in ["venice-text", "venice-conversation-1"] {
        let fixture = load(name);
        assert!(
            String::from_utf8_lossy(&fixture.response).contains(r#""service_tier":"priority""#),
            "{name} no longer shows the case"
        );
        let events = events_of(name);
        assert_eq!(end_of(&events).3.speed, Speed::Standard, "{name}");
        assert_eq!(end_of(&events).3.speed_requested, None, "{name}");
        assert!(fallbacks(&events).is_empty(), "{name}");
    }
    // A request at fast, at a server where fast is another model: it ran
    // at the speed it was sent at.
    let events = events_of("venice-fast");
    assert_eq!(end_of(&events).3.speed, Speed::Fast);
    assert!(fallbacks(&events).is_empty());
    // A model with no tier at all, at a server that names one anyway.
    let named = json!({"id": "c", "model": "m", "service_tier": "flex",
        "choices": [{"index": 0, "delta": {"content": "a"}, "finish_reason": "stop"}]})
    .to_string();
    let events = decode_plain(&[&named, "[DONE]"]);
    assert_eq!(end_of(&events).3.speed, Speed::Standard);
    assert!(fallbacks(&events).is_empty());
}

#[test]
fn what_the_decoder_does_not_know_is_skipped() {
    let (request, provider) = plain();
    let stream = format!(
        ": a comment\n\n\
         data: {first}\n\n\
         event: ping\ndata: {{\"type\":\"ping\"}}\n\n\
         event: provider.note\ndata: not json at all\n\n\
         data: {{\"object\":\"something.new\",\"payload\":[1,2,3]}}\n\n\
         data: \"a bare string\"\n\n\
         data: 42\n\n\
         data: {last}\n\n\
         data: [DONE]\n\n",
        first = chunk(
            &json!({"role": "assistant", "content": "a", "audio": {"id": "x"}}),
            None
        ),
        last = chunk(
            &json!({"content": "b", "logprobs_extra": [1]}),
            Some("stop")
        ),
    );
    let events = decode(&request, &provider, stream.as_bytes(), &[]).unwrap();
    assert_well_formed(&events);
    assert_eq!(text(&events), "ab");
    assert_eq!(end_of(&events).0, &StopReason::End);

    // The keep-alive comments of the recorded servers are among the
    // skipped.
    let recorded = String::from_utf8(load("openrouter-claude-1").response).unwrap();
    assert!(recorded.starts_with(": OPENROUTER PROCESSING"));
}

#[test]
fn a_stream_that_is_damaged_is_an_error_and_not_a_guess() {
    let (request, provider) = plain();
    for (bad, says) in [
        (
            r#"{"id":"c","choices":[{"index":0,"delta":{"content":"#,
            "could not be read",
        ),
        (r#"{"id":"c","choices":"none"}"#, "could not be read"),
        (
            r#"{"id":"c","choices":[{"index":0,"delta":{"content":[{"type":"text","text":"x"}]}}]}"#,
            "neither text nor null",
        ),
        (
            r#"{"id":"c","choices":[{"index":0,"delta":{"tool_calls":"x"}}]}"#,
            "could not be read",
        ),
        (
            r#"{"id":"c","choices":[],"usage":{"prompt_tokens":"many"}}"#,
            "usage",
        ),
    ] {
        let error = decode_events(&request, &provider, &[bad]).unwrap_err();
        assert!(error.contains(says), "{bad}: {error}");
    }
    // A fragment for a call that has ended.
    let call = |arguments: &str| json!({"tool_calls": [{"index": 0, "id": "a", "function": {"name": "r", "arguments": arguments}}]});
    let error = decode_events(
        &request,
        &provider,
        &[
            &chunk(&call("{"), Some("tool_calls")),
            &chunk(&call("}"), None),
        ],
    )
    .unwrap_err();
    assert!(error.contains("has ended"), "{error}");
    // `[DONE]` with no stop reason before it.
    let error = decode_events(
        &request,
        &provider,
        &[&chunk(&json!({"content": "a"}), None), "[DONE]"],
    )
    .unwrap_err();
    assert!(error.contains("without saying why"), "{error}");
}

#[test]
fn a_stream_ends_at_done_or_where_the_server_closes_it_after_a_stop_reason() {
    let (request, provider) = plain();
    let said = chunk(&json!({"content": "a"}), None);
    let stopped = chunk(&json!({}), Some("stop"));
    let usage = json!({"id": "c", "model": "m", "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 1}}).to_string();

    // The end is at `[DONE]`, not at the stop reason: the usage comes in
    // between.
    let events = decode_events(&request, &provider, &[&said, &stopped, &usage, "[DONE]"]).unwrap();
    assert_eq!(end_of(&events).3.tokens.input, 5);
    let before_done = decode_events(&request, &provider, &[&said, &stopped]).unwrap();
    // A server that closes the stream without `[DONE]`: complete, because
    // it gave a stop reason.
    assert_eq!(end_of(&before_done).0, &StopReason::End);
    // A stream cut before any stop reason is not complete, and the client
    // reports it as incomplete: the decoder does not invent an end.
    let cut = decode_events(&request, &provider, &[&said]).unwrap();
    assert!(!cut.iter().any(ModelEvent::is_terminal), "{cut:?}");
    // Nothing at all, or `[DONE]` alone, is no response.
    assert!(decode_events(&request, &provider, &[]).unwrap().is_empty());
    assert!(
        decode_events(&request, &provider, &["[DONE]"])
            .unwrap()
            .is_empty()
    );
    // What follows the end is not read.
    let after = decode_events(&request, &provider, &[&said, &stopped, "[DONE]", &said]).unwrap();
    assert!(after.last().is_some_and(ModelEvent::is_terminal));
}

#[test]
fn text_reasoning_and_calls_that_alternate_are_blocks_in_the_order_they_began() {
    let events = decode_plain(&[
        &chunk(&json!({"reasoning_content": "first"}), None),
        &chunk(&json!({"content": "one"}), None),
        &chunk(&json!({"reasoning_content": "second"}), None),
        &chunk(&json!({"content": "two"}), None),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "a", "function": {"name": "r", "arguments": "{}"}}]}),
            None,
        ),
        &chunk(&json!({"content": "three"}), Some("tool_calls")),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(
        kinds(&events),
        [
            BlockKind::Reasoning,
            BlockKind::Text,
            BlockKind::Reasoning,
            BlockKind::Text,
            BlockKind::ToolCall,
            BlockKind::Text,
        ]
    );
    let indexes: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::BlockStart { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(indexes, [0, 1, 2, 3, 4, 5]);
    // Only the first choice is read.
    let two = json!({"id": "c", "model": "m", "choices": [
        {"index": 1, "delta": {"content": "other"}, "finish_reason": "stop"},
        {"index": 0, "delta": {"content": "mine"}, "finish_reason": "stop"},
    ]})
    .to_string();
    assert_eq!(text(&decode_plain(&[&two, "[DONE]"])), "mine");
}

#[test]
fn the_response_names_itself_and_its_model() {
    let events = events_of("openai-text");
    assert_eq!(
        events[0],
        ModelEvent::Start {
            response_id: Some("chatcmpl-fixture9001".to_owned()),
            model: "gpt-6-luna".to_owned(),
        }
    );
}

// What the review of the step found, each with the stream that showed it.

#[test]
fn an_empty_reasoning_member_is_no_reasoning() {
    // A server that puts an empty list of pieces, or an empty reasoning
    // text, in every chunk beside the content: no reasoning block begins,
    // and the text is one block.
    let events = decode_plain(&[
        &chunk(&json!({"content": "a", "reasoning_details": []}), None),
        &chunk(
            &json!({"content": "b", "reasoning_content": "", "reasoning": ""}),
            None,
        ),
        &chunk(
            &json!({"content": "c", "reasoning_details": []}),
            Some("stop"),
        ),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(kinds(&events), [BlockKind::Text]);
    assert_eq!(text(&events), "abc");
    assert_eq!(raw_blocks(&events), [json!({"content": "abc"})]);
    // An empty first name does not hide a second one that has text.
    let events = decode_plain(&[
        &chunk(&json!({"reasoning_content": "", "reasoning": "hm"}), None),
        &chunk(&json!({"content": "x"}), Some("stop")),
        "[DONE]",
    ]);
    assert_eq!(reasoning(&events), "hm");
    assert_eq!(raw_blocks(&events)[0], json!({"reasoning": "hm"}));
}

#[test]
fn another_id_under_an_index_in_use_is_another_call() {
    // A server that numbers every call of a response 0 and tells them
    // apart by id: joined by index alone, the second call's arguments
    // would be appended to the first's.
    let call = |id: &str, city: &str| {
        chunk(
            &json!({"tool_calls": [{"index": 0, "id": id, "type": "function",
                "function": {"name": "get_weather", "arguments": format!("{{\"city\":\"{city}\"}}")}}]}),
            None,
        )
    };
    let events = decode_plain(&[
        &call("call_a", "Oslo"),
        &call("call_b", "Cairo"),
        &chunk(&json!({}), Some("tool_calls")),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(
        tool_calls(&events),
        [
            (
                "call_a".to_owned(),
                "get_weather".to_owned(),
                r#"{"city":"Oslo"}"#.to_owned()
            ),
            (
                "call_b".to_owned(),
                "get_weather".to_owned(),
                r#"{"city":"Cairo"}"#.to_owned()
            ),
        ]
    );
    // A fragment that repeats the id of its call, or has none, is more of
    // that call.
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"name": "r", "arguments": "{\"a\""}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"arguments": ":1"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "", "function": {"arguments": "}"}}]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    assert_eq!(
        tool_calls(&events),
        [("call_a".to_owned(), "r".to_owned(), "{\"a\":1}".to_owned())]
    );
}

#[test]
fn an_id_that_comes_before_the_name_is_the_calls_and_one_that_comes_after_is_not() {
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"arguments": "{"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "call_late"}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"name": "read", "arguments": "}"}}]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    assert_eq!(
        tool_calls(&events),
        [("call_late".to_owned(), "read".to_owned(), "{}".to_owned())]
    );
    // Once a call has been announced its id is what was said: a result
    // may already name it.
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"name": "read", "arguments": "{}"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "call_too_late"}]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(
        tool_calls(&events),
        [(
            "call_chatcmpl1_0".to_owned(),
            "read".to_owned(),
            "{}".to_owned()
        )],
        "the late id is neither taken nor the beginning of another call"
    );
    assert_eq!(end_of(&events).0, &StopReason::ToolCalls);
}

#[test]
fn a_call_that_never_gets_a_name_is_left_out_and_said_to_be_malformed() {
    let unnamed = chunk(
        &json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"arguments": "{\"x\":1}"}}]}),
        None,
    );
    for word in ["tool_calls", "stop"] {
        let events = decode_plain(&[&unnamed, &chunk(&json!({}), Some(word)), "[DONE]"]);
        assert_well_formed(&events);
        // No block, no call with an empty name to send back on every
        // later request, and not a turn that ended well.
        assert!(kinds(&events).is_empty(), "{word}: {events:?}");
        assert!(tool_calls(&events).is_empty());
        assert_eq!(end_of(&events).0, &StopReason::MalformedToolCall, "{word}");
        assert_eq!(end_of(&events).1, word);
    }
    // A response cut off by the output limit before the name arrived is
    // the output limit.
    let events = decode_plain(&[&unnamed, &chunk(&json!({}), Some("length")), "[DONE]"]);
    assert_eq!(end_of(&events).0, &StopReason::MaxOutput);
    // The calls that did get a name are still there, and still the turn
    // does not go on as if all were well.
    let named = chunk(
        &json!({"tool_calls": [{"index": 1, "id": "call_b", "function": {"name": "read", "arguments": "{}"}}]}),
        None,
    );
    let events = decode_plain(&[
        &unnamed,
        &named,
        &chunk(&json!({}), Some("tool_calls")),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(tool_calls(&events).len(), 1);
    assert_eq!(end_of(&events).0, &StopReason::MalformedToolCall);
}

#[test]
fn calls_without_an_index_are_numbered_through_the_whole_response() {
    let whole = |id: &str, n: u32| json!({"id": id, "function": {"name": "read", "arguments": format!("{{\"n\":{n}}}")}});
    // Two in one delta, and a third in the next: three calls, not the
    // third merged into the first.
    let events = decode_plain(&[
        &chunk(&json!({"tool_calls": [whole("a", 1), whole("b", 2)]}), None),
        &chunk(&json!({"tool_calls": [whole("c", 3)]}), Some("tool_calls")),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    let calls = tool_calls(&events);
    let read: Vec<(&str, &str)> = calls
        .iter()
        .map(|(id, _, arguments)| (id.as_str(), arguments.as_str()))
        .collect();
    assert_eq!(
        read,
        [("a", "{\"n\":1}"), ("b", "{\"n\":2}"), ("c", "{\"n\":3}")]
    );
    // The same without ids: the place in the response is all there is.
    let bare =
        |n: u32| json!({"function": {"name": "read", "arguments": format!("{{\"n\":{n}}}")}});
    let events = decode_plain(&[
        &chunk(&json!({"tool_calls": [bare(1)]}), None),
        &chunk(&json!({"tool_calls": [bare(2)]}), Some("tool_calls")),
        "[DONE]",
    ]);
    let calls = tool_calls(&events);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].2, "{\"n\":2}");
    assert_ne!(calls[0].0, calls[1].0);
}

#[test]
fn pieces_of_reasoning_are_joined_by_type_and_index_and_an_opaque_one_is_never_shown() {
    let piece = |pieces: Value| chunk(&json!({"reasoning_details": pieces}), None);
    let events = decode_plain(&[
        &piece(
            json!([{"type": "reasoning.summary", "summary": "First, ", "id": "rs_1", "format": "f", "index": 0}]),
        ),
        &piece(json!([{"type": "reasoning.summary", "summary": "the sum.", "index": 0}])),
        // The same index under another type is another piece.
        &piece(
            json!([{"type": "reasoning.encrypted", "data": "b3BhcXVl", "id": "rs_2", "index": 0}]),
        ),
        &piece(json!([{"type": "reasoning.encrypted", "data": "LW1vcmU=", "index": 0}])),
        // Another index is another piece.
        &piece(json!([{"type": "reasoning.text", "text": "Then ", "signature": null, "index": 1}])),
        &piece(
            json!([{"type": "reasoning.text", "text": "the rest.", "index": 1},
                      {"type": "reasoning.text", "signature": "c2ln", "index": 1}]),
        ),
        &chunk(&json!({"content": "40"}), Some("stop")),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(kinds(&events), [BlockKind::Reasoning, BlockKind::Text]);
    // The summary and the text are shown. The opaque payload is not.
    assert_eq!(reasoning(&events), "First, the sum.Then the rest.");
    assert_eq!(
        raw_blocks(&events)[0],
        json!({"reasoning_details": [
            {"type": "reasoning.summary", "summary": "First, the sum.", "id": "rs_1", "format": "f", "index": 0},
            {"type": "reasoning.encrypted", "data": "b3BhcXVlLW1vcmU=", "id": "rs_2", "index": 0},
            {"type": "reasoning.text", "text": "Then the rest.", "signature": "c2ln", "index": 1},
        ]})
    );
    // Without an index a piece continues the last one of its type, and
    // begins a new one after a piece of another type.
    let events = decode_plain(&[
        &piece(json!([{"type": "reasoning.text", "text": "a"}])),
        &piece(json!([{"type": "reasoning.text", "text": "b"}])),
        &piece(json!([{"type": "reasoning.summary", "summary": "c"}])),
        &piece(json!([{"type": "reasoning.text", "text": "d"}])),
        &chunk(&json!({"content": "x"}), Some("stop")),
        "[DONE]",
    ]);
    assert_eq!(
        raw_blocks(&events)[0]["reasoning_details"],
        json!([
            {"type": "reasoning.text", "text": "ab"},
            {"type": "reasoning.summary", "summary": "c"},
            {"type": "reasoning.text", "text": "d"},
        ])
    );
}

#[test]
fn the_first_stop_reason_stands_and_an_empty_one_is_none() {
    // A server that writes an empty stop reason where others write null.
    let events = decode_plain(&[
        &chunk(&json!({"content": "a"}), Some("")),
        &chunk(&json!({"content": "b"}), Some("")),
        &chunk(&json!({}), Some("length")),
        // And one that says something else with the usage.
        &chunk(&json!({}), Some("stop")),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    assert_eq!(
        kinds(&events),
        [BlockKind::Text],
        "an empty reason ends no block"
    );
    assert_eq!(text(&events), "ab");
    let (stop, raw, _, _) = end_of(&events);
    assert_eq!((stop, raw), (&StopReason::MaxOutput, "length"));
}

#[test]
fn text_held_back_for_a_tag_is_handed_on_before_a_call_begins() {
    let mut request = local(
        providers::LMSTUDIO,
        "qwen/qwen3-8b",
        r#"{"flags": {"reasoning_tags": true}}"#,
    );
    request.tools = Vec::new();
    let provider = provider(providers::LMSTUDIO);
    // The text ends on what could be the beginning of the opening tag,
    // and then a call begins.
    let events = decode_events(
        &request,
        &provider,
        &[
            &chunk(&json!({"content": "<thi"}), None),
            &chunk(
                &json!({"tool_calls": [{"index": 0, "id": "a", "function": {"name": "r", "arguments": "{}"}}]}),
                Some("tool_calls"),
            ),
            "[DONE]",
        ],
    )
    .unwrap();
    assert_well_formed(&events);
    assert_eq!(kinds(&events), [BlockKind::Text, BlockKind::ToolCall]);
    assert_eq!(text(&events), "<thi");
    // The text block ended before the call began.
    let ended = events
        .iter()
        .position(|event| matches!(event, ModelEvent::BlockEnd { index: 0, .. }))
        .unwrap();
    let called = events
        .iter()
        .position(|event| matches!(event, ModelEvent::ToolCallStart { .. }))
        .unwrap();
    assert!(ended < called);
}

#[test]
fn a_context_that_is_too_long_is_told_apart_inside_a_response_too() {
    // No status and no code: only the sentence.
    let events = decode_plain(&[
        r#"{"error":{"message":"This model's maximum context length is 4096 tokens."}}"#,
    ]);
    assert_eq!(error_of(&events).kind, ErrorKind::ContextOverflow);
    // Any other sentence is a failure of the server's side.
    let events = decode_plain(&[r#"{"error":{"message":"something broke"}}"#]);
    assert_eq!(error_of(&events).kind, ErrorKind::Transient);
}

#[test]
fn a_made_up_call_id_is_short_whatever_the_responses_id_is() {
    // Some servers refuse a call id over 40 characters, and a history may
    // be handed to one of them later.
    let long = json!({"id": "chatcmpl-0123456789abcdefghijklmnopqrstuvwxyzABCDEFGH",
        "choices": [{"index": 0, "finish_reason": "tool_calls", "delta":
        {"tool_calls": [{"index": 0, "function": {"name": "read", "arguments": "{}"}}]}}]})
    .to_string();
    let made = tool_calls(&decode_plain(&[&long, "[DONE]"]))[0].0.clone();
    assert_eq!(made, "call_stuvwxyzABCDEFGH_0");
    assert!(made.len() <= 40);
}

// What the second review of the step found.

#[test]
fn what_a_failed_response_used_is_counted() {
    // DeepSeek's failing stop reason comes with the usage, in one chunk.
    let events = events_of("deepseek-insufficient-resource");
    let used: Vec<&cortex_llm::Usage> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::Usage(usage) => Some(usage),
            _ => None,
        })
        .collect();
    assert_eq!(used.len(), 1, "{events:?}");
    assert_eq!((used[0].tokens.input, used[0].tokens.output), (19, 3));
    assert!(matches!(events.last(), Some(ModelEvent::Error(_))));
    // And an `error` chunk that carries one.
    let events = decode_plain(&[
        &chunk(&json!({"content": "a"}), None),
        r#"{"id":"c","error":{"code":502,"message":"gone"},"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":1}}"#,
    ]);
    let position = |wanted: fn(&ModelEvent) -> bool| events.iter().position(wanted).unwrap();
    let usage = position(|event| matches!(event, ModelEvent::Usage(_)));
    let error = position(|event| matches!(event, ModelEvent::Error(_)));
    assert!(
        usage < error,
        "the count comes before the end of the stream"
    );
    assert_eq!(error, events.len() - 1);
}

#[test]
fn a_tool_call_cut_off_by_the_output_limit_is_the_output_limit() {
    // Recorded from two servers: the call was named, its arguments stop in
    // the middle, and the response says `length`.
    for name in ["deepseek-tool-call-cut", "venice-tool-call-cut"] {
        let events = events_of(name);
        assert_well_formed(&events);
        let calls = tool_calls(&events);
        assert_eq!(calls.len(), 1, "{name}");
        assert_eq!(calls[0].1, "get_weather", "{name}");
        // What arrived of the arguments is handed on as the model wrote
        // it, and is not JSON.
        assert!(
            calls[0].2.starts_with("{\"city\""),
            "{name}: {}",
            calls[0].2
        );
        assert!(
            serde_json::from_str::<Value>(&calls[0].2).is_err(),
            "{name}"
        );
        let (stop, raw, _, _) = end_of(&events);
        assert_eq!((stop, raw), (&StopReason::MaxOutput, "length"), "{name}");
        // The stored block holds the text of the arguments in a string,
        // so it is one JSON object whatever the model wrote.
        let block = raw_blocks(&events).pop().unwrap();
        assert_eq!(
            block["tool_calls"][0]["function"]["arguments"],
            calls[0].2.as_str()
        );
    }
    // Recorded from OpenRouter: two calls arrived whole, and an upstream
    // host left the third, which the output limit cut off, in the text,
    // half-written. The two whole calls are calls, the stop is for them,
    // and the server's own word for the stop says what happened.
    let events = events_of("openrouter-tool-call-cut");
    assert_well_formed(&events);
    let cities: Vec<Value> = tool_calls(&events)
        .iter()
        .map(|(_, _, arguments)| serde_json::from_str::<Value>(arguments).unwrap()["city"].clone())
        .collect();
    assert_eq!(cities, ["Paris", "Tokyo"]);
    assert!(text(&events).contains("<tool_call>"), "{}", text(&events));
    let (stop, raw, native, _) = end_of(&events);
    assert_eq!(
        (stop, raw, native),
        (&StopReason::ToolCalls, "tool_calls", Some("length"))
    );
    // The same stop with no call that arrived whole: there is nothing to
    // run, and the server's own word says the output limit is why.
    let cut = json!({"id": "c", "model": "m", "choices": [{"index": 0,
        "delta": {"content": "<tool_call>get_weather"},
        "finish_reason": "tool_calls", "native_finish_reason": "length"}]})
    .to_string();
    let events = decode_plain(&[&cut, "[DONE]"]);
    assert_eq!(end_of(&events).0, &StopReason::MaxOutput);
}

#[test]
fn a_stop_reason_nobody_knows_is_passed_on_from_a_file_too() {
    let events = events_of("deepseek-stop-aborted");
    let (stop, raw, _, usage) = end_of(&events);
    assert!(!stop.is_known());
    assert_eq!((stop.as_str(), raw), ("openai:aborted", "aborted"));
    assert_eq!(text(&events), "Hello to");
    assert_eq!(usage.tokens.input, 19);
    // And the stream with things in it nobody knows.
    let events = events_of("openai-unknown-event");
    assert_eq!(kinds(&events), [BlockKind::Text]);
    assert_eq!(text(&events), "Hello to the test suite.");
    assert_eq!(end_of(&events).0, &StopReason::End);
}

#[test]
fn a_server_that_numbers_nothing_may_still_send_a_call_in_pieces() {
    // No index anywhere: the first item names the call, and the items
    // after it that begin nothing are more of it.
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{"id": "a", "function": {"name": "read", "arguments": "{\"pa"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"function": {"arguments": "th\":"}}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"function": {"arguments": "1}"}}]}),
            None,
        ),
        // An item with a name is the next call.
        &chunk(
            &json!({"tool_calls": [{"function": {"name": "list", "arguments": "{}"}}]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    assert_well_formed(&events);
    let calls = tool_calls(&events);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(
        (calls[0].0.as_str(), calls[0].2.as_str()),
        ("a", "{\"path\":1}")
    );
    assert_eq!((calls[1].1.as_str(), calls[1].2.as_str()), ("list", "{}"));
    assert_eq!(end_of(&events).0, &StopReason::ToolCalls);
}

#[test]
fn what_a_later_fragment_of_a_call_carries_goes_back_with_it_too() {
    let events = decode_plain(&[
        &chunk(
            &json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
                "function": {"name": "read", "arguments": "{"}, "kept": 1}]}),
            None,
        ),
        &chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"arguments": "}"},
                "extra_content": {"vendor": {"thought_signature": "c2ln"}}, "kept": 2, "gone": null}]}),
            Some("tool_calls"),
        ),
        "[DONE]",
    ]);
    assert_eq!(
        raw_blocks(&events)[0],
        json!({"tool_calls": [{
            "id": "call_a", "type": "function",
            "function": {"name": "read", "arguments": "{}"},
            "kept": 2,
            "extra_content": {"vendor": {"thought_signature": "c2ln"}},
        }]})
    );
}

#[test]
fn the_cache_counts_are_read_from_where_each_recorded_server_puts_them() {
    // Venice.ai writes the count of reads beside the details as well as
    // inside them. Both say the same, and neither is counted twice.
    let events = events_of("venice-conversation-2");
    let usage = end_of(&events).3;
    let raw: Value = serde_json::from_str(usage.raw.as_ref().unwrap().as_str()).unwrap();
    assert_eq!(
        raw["cache_read_input_tokens"],
        raw["prompt_tokens_details"]["cached_tokens"]
    );
    assert_eq!(
        usage.tokens.cache_read,
        raw["cache_read_input_tokens"].as_u64().unwrap()
    );
    assert_eq!(
        usage.tokens.input + usage.tokens.cache_read + usage.tokens.cache_write,
        raw["prompt_tokens"].as_u64().unwrap()
    );
}
