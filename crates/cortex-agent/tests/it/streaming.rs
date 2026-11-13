//! A response becomes deltas and then one item, and each stop reason does
//! what its row says (`docs/architecture/03-agent-loop.md`, "Streaming"
//! and "Stop reasons").

use std::sync::Arc;
use std::time::Duration;

use cortex_agent::{Record, StatEvent, ToolStatOutcome};
use cortex_llm::{BlockKind, ModelEvent};
use cortex_protocol::event::{Event, TurnOutcome, Verification};
use cortex_protocol::item::ToolOutcome;
use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{ErrorKind, Item, Part, StopReason};
use cortex_testkit::{FakeTimer, Response};
use serde_json::json;

use crate::transitions::refused;
use crate::{Rig, Setup, World, assert_valid_history, calls, last_request, says, text_of};

fn turn(world: &World, rig: &Rig, turns: usize) {
    world.run(async {
        rig.prompt("go");
        rig.events.turns(turns).await;
    });
}

#[test]
fn a_response_is_deltas_then_one_item_that_is_recorded_once() {
    let tokens = TokenUsage {
        input: 1_000,
        output: 200,
        ..TokenUsage::default()
    };
    let response = Response::new()
        .reasoning("Think first.")
        .text_in(&["Hel", "lo."])
        .tokens(tokens.clone())
        .end(StopReason::End);
    let (world, rig) = Rig::one("stream_one_item", [response]);
    turn(&world, &rig, 1);

    assert_eq!(
        rig.events.labels(),
        [
            "turn.started",
            "user",
            "started:assistant",
            "delta:reasoning",
            "delta:text",
            "delta:text",
            "assistant",
            "turn.completed:completed",
            "idle"
        ]
    );

    // `seq` goes up by one, and events of a turn name the turn.
    let events = rig.events.all();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1);
        assert_eq!(event.session_id.as_str(), "s_1");
    }
    let idle = events.last().unwrap();
    assert!(matches!(idle.event, Event::SessionIdle(_)));
    assert_eq!(idle.turn_id, None, "idle is outside any turn");
    assert_eq!(events[0].turn_id.as_ref().unwrap().as_str(), "t_1");

    // The item is authoritative: both blocks, each with the provider's own
    // JSON, the stop reason, the origin and the usage.
    let items = rig.events.items();
    let [Item::UserMessage(user), Item::AssistantMessage(assistant)] = items.as_slice() else {
        panic!("{items:#?}");
    };
    assert_eq!((user.id.as_str(), assistant.id.as_str()), ("i_1", "i_2"));
    let [Part::Reasoning(reasoning), Part::Text(text)] = assistant.parts.as_slice() else {
        panic!("{:#?}", assistant.parts);
    };
    assert_eq!(reasoning.text, "Think first.");
    assert!(
        reasoning
            .raw
            .as_ref()
            .unwrap()
            .as_str()
            .contains("signature")
    );
    assert_eq!(text.text, "Hello.");
    assert!(text.raw.is_some());
    assert_eq!(assistant.stop_reason, Some(StopReason::End));
    assert_eq!(assistant.raw_stop.as_deref(), Some("end"));
    assert_eq!(
        assistant.origin.as_ref().unwrap().model.as_str(),
        "test-model"
    );
    let usage = assistant.usage.as_ref().unwrap();
    assert_eq!(usage.tokens, tokens);
    // 1,000 input at $4 and 200 output at $20 per million.
    assert_eq!(usage.cost.as_ref().unwrap().usd_nanos, 8_000_000);
    assert_eq!(usage.catalog_version.as_deref(), Some("test-1"));

    // The log holds exactly what front ends were shown as complete.
    assert_eq!(rig.store.items(), items);
    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Completed);
    assert_eq!(ends[0].stop_reason, Some(StopReason::End));
    assert_eq!(ends[0].usage.requests, 1);
    assert_eq!(ends[0].usage.tokens.input, 1_000);
    assert_eq!(ends[0].verification, Some(Verification::Skipped));
    assert_eq!(rig.store.outcomes().len(), 1);

    // The request had the system prompt, the tools and the prompt.
    let request = last_request(&world.provider);
    assert_eq!(request.system, "You are a test.");
    assert_eq!(request.tools.len(), 7);
    assert_eq!(text_of(&request.messages[0]), "go");
    assert!(request.cache.stable_prefix && request.cache.history);

    // And the session's usage has it.
    let session = rig.handle.usage();
    assert_eq!(session.totals.requests, 1);
    assert_eq!(session.context_tokens, Some(1_200));
    assert_eq!(session.last_request.unwrap().tokens, tokens);
}

#[test]
fn usage_updates_are_sent_at_most_once_a_frame_and_once_at_the_end() {
    let tokens = TokenUsage {
        input: 10,
        output: 5,
        ..TokenUsage::default()
    };
    // Three running reports with no time between them, then two more a
    // frame apart.
    let response = Response::new()
        .tokens(tokens)
        .usage()
        .usage()
        .usage()
        .delay(Duration::from_millis(20))
        .usage()
        .delay(Duration::from_millis(20))
        .usage()
        .text("Done.")
        .end(StopReason::End);
    let world = World::new("usage_throttle", []);
    let timer = FakeTimer::auto(&world.clock);
    let provider = cortex_testkit::ScriptedProvider::new([response]).with_timer(Arc::new(timer));
    let mut setup = Setup::new(&world);
    setup.model.client = Arc::new(provider);
    let rig = setup.start(&world);
    turn(&world, &rig, 1);

    let sent = rig
        .events
        .every_label()
        .iter()
        .filter(|label| *label == "usage")
        .count();
    assert_eq!(
        sent, 4,
        "one for the burst, one per later frame, one at the end"
    );
}

#[test]
fn arguments_that_are_not_json_get_an_error_result_and_the_round_goes_on() {
    let broken = Response::new()
        .tool_call_in("c1", "read", &["{\"path\": "])
        .tool_call("c2", "read", &json!({"path": "b"}))
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("invalid_json", [broken, says("Sorry.")]);
    turn(&world, &rig, 1);

    let items = rig.store.items();
    let [
        _,
        Item::AssistantMessage(assistant),
        Item::ToolResult(first),
        Item::ToolResult(second),
        _,
    ] = items.as_slice()
    else {
        panic!("{items:#?}");
    };
    let Part::ToolCall(call) = &assistant.parts[0] else {
        panic!("{:#?}", assistant.parts);
    };
    assert_eq!(
        call.arguments,
        json!("{\"path\": "),
        "the model's text, as a string"
    );
    assert_eq!(first.outcome, ToolOutcome::Error);
    assert!(
        text_of_result(first).contains("not valid JSON"),
        "{first:#?}"
    );
    assert_eq!(second.outcome, ToolOutcome::Success);
    assert_eq!(
        world.log.lines(),
        ["start read b", "end read b"],
        "the broken call never ran"
    );
    assert_valid_history(&last_request(&world.provider).messages);

    let outcomes: Vec<ToolStatOutcome> = rig
        .stats
        .all()
        .iter()
        .filter_map(|event| match event {
            StatEvent::ToolCall(call) => Some(call.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(
        outcomes,
        [ToolStatOutcome::InvalidArguments, ToolStatOutcome::Ok]
    );
}

fn text_of_result(result: &cortex_protocol::item::ToolResultItem) -> String {
    result
        .content
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_tool_that_does_not_exist_and_arguments_that_do_not_fit_are_errors_for_the_model() {
    let response = calls(&[
        ("c1", "teleport", json!({})),
        ("c2", "read", json!({"path": "a", "bad": true})),
    ]);
    let (world, rig) = Rig::one("unknown_tool", [response, says("I see.")]);
    turn(&world, &rig, 1);

    let results: Vec<_> = rig
        .store
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].outcome, ToolOutcome::Error);
    let unknown = text_of_result(&results[0]);
    assert!(unknown.contains("There is no tool `teleport`"), "{unknown}");
    assert!(
        unknown.contains("read") && unknown.contains("shell"),
        "it lists the tools: {unknown}"
    );
    assert_eq!(
        text_of_result(&results[1]),
        "`bad` is not an argument of this tool."
    );
    assert!(world.log.lines().is_empty(), "nothing ran");
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn a_tool_call_that_could_not_be_read_is_told_to_the_model_and_the_turn_goes_on() {
    let malformed = Response::new()
        .text("Let me look.")
        .end(StopReason::MalformedToolCall);
    let (world, rig) = Rig::one("malformed", [malformed, says("Here.")]);
    turn(&world, &rig, 1);

    assert_eq!(
        rig.events.labels(),
        [
            "turn.started",
            "user",
            "started:assistant",
            "delta:text",
            "assistant",
            "note:harness",
            "started:assistant",
            "delta:text",
            "assistant",
            "turn.completed:completed",
            "idle"
        ]
    );
    let request = last_request(&world.provider);
    assert!(text_of(request.messages.last().unwrap()).contains("could not be read"));
    assert_valid_history(&request.messages);
}

#[test]
fn a_response_cut_off_in_its_text_ends_the_turn_marked_truncated() {
    let cut = Response::new()
        .text("The answer is")
        .end(StopReason::MaxOutput);
    let (world, rig) = Rig::one("max_output_text", [cut]);
    turn(&world, &rig, 1);

    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Failed);
    assert_eq!(ends[0].stop_reason, Some(StopReason::MaxOutput));
    let error = ends[0].error.as_ref().unwrap();
    assert_eq!(
        (error.kind.clone(), error.code.as_str()),
        (ErrorKind::ModelStop, "max_output")
    );
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("an assistant item");
    };
    assert!(assistant.truncated);
    assert_eq!(assistant.error, None, "the response itself did not fail");
}

/// A response whose last block is a tool call that never finished.
fn cut_off_in_a_call() -> Response {
    Response::new()
        .text("Writing it.")
        .event(ModelEvent::BlockStart {
            index: 7,
            kind: BlockKind::ToolCall,
        })
        .event(ModelEvent::ToolCallStart {
            index: 7,
            id: "c1".to_owned(),
            name: "write".to_owned(),
        })
        .event(ModelEvent::ToolArgsDelta {
            index: 7,
            json: "{\"path\": \"a\", \"content\": \"long".to_owned(),
        })
        .end(StopReason::MaxOutput)
}

#[test]
fn a_response_cut_off_inside_a_tool_call_is_sent_once_more_with_more_room() {
    let world = World::new(
        "max_output_call",
        [
            cut_off_in_a_call(),
            calls(&[("c2", "write", json!({"path": "a"}))]),
            says("Written."),
        ],
    );
    let mut setup = Setup::new(&world);
    setup.model.max_output = Some(1_000);
    let rig = setup.start(&world);
    turn(&world, &rig, 1);

    let limits: Vec<Option<u64>> = world
        .provider
        .requests()
        .iter()
        .map(|request| request.max_output)
        .collect();
    assert_eq!(
        limits,
        [Some(1_000), None, Some(1_000)],
        "raised once, for that request"
    );
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert!(rig.events.labels().contains(&"reset".to_owned()));

    // The cut-off response is an attempt in the log, not an item.
    let attempts = rig.store.attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].error.code, "max_output");
    assert_eq!(
        attempts[0].partial.len(),
        1,
        "the text; the unfinished call is no part"
    );
    // The two requests after it saw the same history: nothing of the
    // cut-off response.
    let requests = world.provider.requests();
    assert_eq!(requests[0].messages, requests[1].messages);
    assert_eq!(world.log.count("start write a"), 1);
}

#[test]
fn with_no_room_to_raise_the_cut_off_call_is_dropped_and_the_turn_ends() {
    // The model's own maximum is already the limit.
    let (world, rig) = Rig::one("max_output_no_room", [cut_off_in_a_call()]);
    turn(&world, &rig, 1);

    assert_eq!(world.provider.requests().len(), 1);
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("an assistant item");
    };
    assert!(assistant.truncated);
    assert_eq!(assistant.parts.len(), 1, "the unfinished call is left out");
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Failed);
    assert!(world.log.lines().is_empty());
}

#[test]
fn a_refusal_ends_the_turn_and_runs_none_of_its_calls() {
    let refusal = Response::new()
        .text("I can't help with that.")
        .tool_call("c1", "write", &json!({"path": "a"}))
        .end(StopReason::Refusal);
    let filtered = Response::new().text("...").end(StopReason::ContentFilter);
    let (world, rig) = Rig::one("refusal", [refusal, filtered]);
    turn(&world, &rig, 1);

    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Failed);
    assert_eq!(ends[0].stop_reason, Some(StopReason::Refusal));
    let error = ends[0].error.as_ref().unwrap();
    assert_eq!(
        (error.kind.clone(), error.code.as_str()),
        (ErrorKind::ModelStop, "refusal")
    );
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("an assistant item");
    };
    assert_eq!(
        assistant.parts.len(),
        1,
        "its tool call is not kept: {:#?}",
        assistant.parts
    );
    assert!(world.log.lines().is_empty(), "and never ran");

    // The history is still one a request accepts, and a content filter
    // ends a turn the same way.
    world.run(async {
        rig.prompt("then this");
        rig.events.turns(2).await;
    });
    assert_valid_history(&last_request(&world.provider).messages);
    let error = rig.events.ends()[1].error.clone().unwrap();
    assert_eq!(error.code, "content_filter");
}

#[test]
fn a_request_that_fails_part_way_keeps_what_arrived_with_the_error_attached() {
    let response = Response::new()
        .text("Half an ans")
        .tool_call("c1", "read", &json!({"path": "a"}))
        .error(refused());
    let (world, rig) = Rig::one("failure_keeps_partial", [response, says("Again.")]);
    turn(&world, &rig, 1);

    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Failed);
    assert_eq!(ends[0].stop_reason, Some(StopReason::Error));
    assert_eq!(ends[0].error.as_ref().unwrap().code, "invalid_request");
    let items = rig.store.items();
    let [_, Item::AssistantMessage(assistant)] = items.as_slice() else {
        panic!("{items:#?}");
    };
    assert_eq!(assistant.stop_reason, Some(StopReason::Error));
    assert_eq!(assistant.error.as_ref().unwrap().kind, ErrorKind::Provider);
    let [Part::Text(text)] = assistant.parts.as_slice() else {
        panic!(
            "the text is kept and the call is not: {:#?}",
            assistant.parts
        );
    };
    assert_eq!(text.text, "Half an ans");

    let failed = rig.stats.all().into_iter().find_map(|event| match event {
        StatEvent::Request(request) => Some(request),
        _ => None,
    });
    let failed = failed.unwrap();
    assert_eq!(failed.stop, StopReason::Error);
    assert_eq!(failed.error.unwrap().code, "invalid_request");

    world.run(async {
        rig.prompt("once more");
        rig.events.turns(2).await;
    });
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn a_stream_that_just_stops_is_a_failed_request_not_a_hung_turn() {
    let (world, rig) = Rig::one("stream_aborts", [Response::new().text("Half").abort()]);
    turn(&world, &rig, 1);
    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Failed);
    assert_eq!(ends[0].error.as_ref().unwrap().code, "incomplete_stream");

    // So is a script with nothing left: the provider's own error ends it.
    let (world, rig) = Rig::one("script_exhausted", []);
    turn(&world, &rig, 1);
    assert_eq!(
        rig.events.ends()[0].error.as_ref().unwrap().code,
        "script_exhausted"
    );
}

#[test]
fn content_with_no_start_event_is_still_a_response() {
    let response = Response::events([ModelEvent::TextDelta {
        index: 0,
        text: "Unannounced.".to_owned(),
    }])
    .end(StopReason::End);
    let (world, rig) = Rig::one("no_start", [response]);
    turn(&world, &rig, 1);
    assert_eq!(
        rig.events.labels()[2..5],
        ["started:assistant", "delta:text", "assistant"]
    );
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
}

#[test]
fn a_running_tools_output_reaches_front_ends_as_deltas() {
    let response = calls(&[("c1", "read", json!({"path": "a", "output": "reading...\n"}))]);
    let (world, rig) = Rig::one("tool_output", [response, says("Done.")]);
    turn(&world, &rig, 1);
    let labels = rig.events.labels();
    let started = rig.events.position("started:read");
    assert_eq!(
        rig.events.every_label()[started..started + 3],
        ["started:read", "delta:tool_output", "result:read:success"],
        "{labels:?}"
    );
}

#[test]
fn every_record_of_a_turn_is_filed_under_the_turn() {
    let (world, rig) = Rig::one(
        "records_have_turns",
        [
            calls(&[("c1", "read", json!({"path": "a"}))]),
            says("Done."),
        ],
    );
    turn(&world, &rig, 1);
    let records = rig.store.records();
    assert_eq!(records.len(), 4);
    assert!(records.iter().all(|(turn, _)| turn.as_str() == "t_1"));
    let kinds: Vec<&str> = records
        .iter()
        .map(|(_, record)| match record {
            Record::Item(Item::UserMessage(_)) => "user",
            Record::Item(Item::AssistantMessage(_)) => "assistant",
            Record::Item(Item::ToolResult(_)) => "result",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["user", "assistant", "result", "assistant"]);
}

#[test]
fn a_second_call_with_the_id_of_the_first_is_not_a_call() {
    // A result names the call it answers by id, so two calls with one id
    // cannot both be answered. The second is dropped from the message and
    // never runs.
    let response = calls(&[
        ("same", "read", json!({"path": "a"})),
        ("same", "write", json!({"path": "b"})),
        ("other", "read", json!({"path": "c"})),
    ]);
    let (world, rig) = Rig::one("duplicate_id", [response, says("Done.")]);
    turn(&world, &rig, 1);

    let items = rig.store.items();
    let Item::AssistantMessage(assistant) = &items[1] else {
        panic!("an assistant item");
    };
    let ids: Vec<&str> = assistant
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["same", "other"]);
    assert_eq!(world.log.count("start write b"), 0);
    let results = items
        .iter()
        .filter(|item| matches!(item, Item::ToolResult(_)))
        .count();
    assert_eq!(results, 2);
    assert_valid_history(&last_request(&world.provider).messages);
}
