//! Interruption (`docs/architecture/02-runtime.md`, "Cancellation";
//! `docs/architecture/03-agent-loop.md`, "Interruption").
//!
//! The rule under test: **after an interruption, in whatever state it
//! came, the history is one the next request accepts.** Each case
//! interrupts a turn the moment it enters a state, then sends another
//! prompt and checks the request that goes out the way a provider would.

use cortex_agent::{Decision, TurnState};
use cortex_protocol::error::code;
use cortex_protocol::event::{TurnOutcome, Verification};
use cortex_protocol::item::{ToolOutcome, ToolResultItem};
use cortex_protocol::{Item, Part, StopReason};
use cortex_testkit::{Gate, Response};
use serde_json::json;

use crate::transitions::{retry, start};
use crate::{Rig, Setup, World, assert_valid_history, calls, last_request, lock, says, text_of};

fn results(rig: &Rig) -> Vec<ToolResultItem> {
    rig.store
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect()
}

/// Sends a second prompt and checks the request it makes.
fn assert_the_next_request_is_accepted(world: &World, rig: &Rig) {
    world.provider.push(says("After the interruption."));
    world.run(async {
        rig.prompt("and now?");
        let turns = rig.events.ends().len() + 1;
        rig.events.turns(turns).await;
    });
    let request = last_request(&world.provider);
    assert_valid_history(&request.messages);
    assert_eq!(text_of(request.messages.last().unwrap()), "and now?");
    assert_eq!(
        rig.events.ends().last().unwrap().outcome,
        TurnOutcome::Completed,
        "the session carries on"
    );
}

/// Runs a prompt with the turn interrupted when it enters `state` for the
/// `entry`th time (counted from 0), and returns once the turn has ended.
fn interrupt_in(world: &World, rig: &Rig, state: TurnState, entry: usize) {
    rig.seen.interrupt_at_entry(state, entry, &rig.handle);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert!(
        rig.seen.states().contains(&state),
        "the turn never entered {state}: {:?}",
        rig.seen.states()
    );
}

fn assert_interrupted(rig: &Rig) {
    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Interrupted);
    assert_eq!(end.stop_reason, Some(StopReason::Interrupted));
    assert_eq!(end.verification, None);
    assert_eq!(rig.handle.state().running_turn, None);
}

#[test]
fn interrupted_while_preparing_the_first_request() {
    let (world, rig) = Rig::one("int_preparing", []);
    interrupt_in(&world, &rig, TurnState::Preparing, 0);
    assert_interrupted(&rig);
    // The prompt is kept: it is what exists.
    assert_eq!(rig.store.items().len(), 1);
    assert!(world.provider.requests().is_empty());
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_while_preparing_after_a_tool_round() {
    let (world, rig) = Rig::one(
        "int_preparing_again",
        [calls(&[("c1", "read", json!({"path": "a"}))])],
    );
    interrupt_in(&world, &rig, TurnState::Preparing, 1);
    assert_interrupted(&rig);
    assert_eq!(results(&rig).len(), 1, "the round before it is whole");
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_while_compacting() {
    // Interrupted as it enters the state: the summary is never asked for.
    let (world, rig) = Rig::one("int_compacting", []);
    rig.window(4_000);
    rig.bulk(6_000);
    interrupt_in(&world, &rig, TurnState::Compacting, 0);
    assert_interrupted(&rig);
    assert!(
        rig.store.summaries().is_empty() && rig.store.epochs().len() == 1,
        "nothing of the compaction was recorded"
    );
    // The history is whole: with room, the next request goes as it is.
    rig.window(200_000);
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_before_the_response_starts() {
    let (world, rig) = Rig::one("int_requesting", [Response::events([]).stall()]);
    world.run(async {
        rig.prompt("go");
        // The request has gone out and nothing has come back.
        while world.provider.requests().is_empty() {
            tokio::task::yield_now().await;
        }
        assert_eq!(rig.handle.turn_state(), TurnState::Requesting);
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
    assert_interrupted(&rig);
    assert_eq!(
        rig.store.items().len(),
        1,
        "no assistant item: nothing arrived"
    );
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_while_text_is_streaming_keeps_the_text_marked_interrupted() {
    let (world, rig) = Rig::one(
        "int_streaming_text",
        [Response::new().text("Half an ans").stall()],
    );
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        assert!(rig.handle.interrupt().interrupted);
        rig.events.turns(1).await;
    });
    assert_interrupted(&rig);
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("the partial message is recorded");
    };
    assert_eq!(assistant.stop_reason, Some(StopReason::Interrupted));
    let [Part::Text(text)] = assistant.parts.as_slice() else {
        panic!("{:#?}", assistant.parts);
    };
    assert_eq!(text.text, "Half an ans");
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_right_as_the_response_starts() {
    // Nothing has arrived: the item began and has no content. It is in the
    // log, and is not sent.
    let (world, rig) = Rig::one("int_streaming_empty", [Response::new().stall()]);
    interrupt_in(&world, &rig, TurnState::Streaming, 0);
    assert_interrupted(&rig);
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("the item that had begun is completed");
    };
    assert!(assistant.parts.is_empty());
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_mid_stream_with_one_call_complete_one_running_and_one_half_written() {
    let response = Response::new()
        .text("Three things.")
        // Complete, free of side effects: it starts early and is running.
        .tool_call("c1", "read", &json!({"path": "a", "gate": "never"}))
        // Complete, with a side effect: it waits for the end, which never
        // comes.
        .tool_call("c2", "write", &json!({"path": "b"}))
        // Half written.
        .event(cortex_llm::ModelEvent::ToolCallStart {
            index: 9,
            id: "c3".to_owned(),
            name: "write".to_owned(),
        })
        .event(cortex_llm::ModelEvent::ToolArgsDelta {
            index: 9,
            json: "{\"path\": \"c".to_owned(),
        })
        .stall();
    let (world, rig) = Rig::one("int_streaming_calls", [response]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        rig.events.seen("delta:tool_args", 5).await;
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
    assert_interrupted(&rig);

    // The message keeps the two complete calls. The half-written one is
    // discarded: its arguments are incomplete.
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("the partial message is recorded");
    };
    let kept: Vec<&str> = assistant
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(kept, ["c1", "c2"]);
    // Each kept call has its one result: the read's own, cut short, and
    // "interrupted" for the write, which never ran.
    let results = results(&rig);
    let outcomes: Vec<(&str, &ToolOutcome)> = results
        .iter()
        .map(|result| (result.call_id.as_str(), &result.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("c1", &ToolOutcome::Interrupted),
            ("c2", &ToolOutcome::Interrupted)
        ]
    );
    assert_eq!(world.log.lines(), ["start read a", "cancelled read a"]);
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_during_the_wait_between_attempts() {
    let response = Response::new().text("Half").event(retry(2)).stall();
    let (world, rig) = Rig::one("int_backoff", [response]);
    interrupt_in(&world, &rig, TurnState::Backoff, 0);
    assert_interrupted(&rig);
    // The attempt that failed is an attempt, not content: the item ends
    // empty and is not sent.
    assert_eq!(rig.store.attempts().len(), 1);
    assert_the_next_request_is_accepted(&world, &rig);
    assert!(!format!("{:?}", last_request(&world.provider).messages).contains("Half"));
}

#[test]
fn interrupted_as_the_response_ends_with_tool_calls() {
    let response = calls(&[
        ("c1", "write", json!({"path": "a"})),
        ("c2", "read", json!({"path": "b", "gate": "never"})),
    ]);
    let (world, rig) = Rig::one("int_deciding", [response]);
    interrupt_in(&world, &rig, TurnState::Deciding, 0);
    assert_interrupted(&rig);
    // The message was complete and is recorded with its calls, so each
    // call gets a result though nothing new may start.
    let results = results(&rig);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].outcome, ToolOutcome::Interrupted);
    assert_eq!(world.log.count("start write a"), 0, "the write never ran");
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_during_a_tool_round_every_call_still_gets_its_result() {
    let response = calls(&[
        ("c1", "read", json!({"path": "a", "gate": "never"})),
        ("c2", "shell", json!({"id": "build"})),
        ("c3", "read", json!({"path": "done"})),
        ("c4", "write", json!({"path": "asked"})),
    ]);
    let world = World::new("int_tool_round", [response]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("write", Decision::Ask);
    rig.policy.wait_at(&world.gates);
    world.run(async {
        rig.prompt("go");
        // One call is running, one is waiting behind it, and a question is
        // open. (The third call waits behind the exclusive second.)
        world.log.has("start read a").await;
        rig.policy.questions(1).await;
        rig.events.seen("assistant", 1).await;
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
    assert_interrupted(&rig);
    let results = results(&rig);
    let outcomes: Vec<(&str, &ToolOutcome)> = results
        .iter()
        .map(|result| (result.call_id.as_str(), &result.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("c1", &ToolOutcome::Interrupted),
            ("c2", &ToolOutcome::Interrupted),
            ("c3", &ToolOutcome::Interrupted),
            // The open question is answered "deny".
            ("c4", &ToolOutcome::Denied),
        ]
    );
    assert_eq!(world.log.lines(), ["start read a", "cancelled read a"]);
    assert_the_next_request_is_accepted(&world, &rig);
    assert_eq!(
        rig.policy.withdrawn(),
        ["write asked"],
        "the question was withdrawn"
    );
}

#[test]
fn a_call_that_finished_before_the_interrupt_keeps_its_own_result() {
    let response = calls(&[
        ("c1", "read", json!({"path": "quick"})),
        ("c2", "read", json!({"path": "slow", "gate": "never"})),
    ]);
    let (world, rig) = Rig::one("int_keeps_finished", [response]);
    world.run(async {
        rig.prompt("go");
        world.log.has("end read quick").await;
        world.log.has("start read slow").await;
        rig.events.seen("assistant", 1).await;
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
    let results = results(&rig);
    assert_eq!(results[0].outcome, ToolOutcome::Success);
    assert_eq!(results[1].outcome, ToolOutcome::Interrupted);
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupted_while_verifying() {
    let (world, rig) = Rig::one(
        "int_verifying",
        [
            calls(&[("c1", "write", json!({"path": "a"}))]),
            says("Done."),
        ],
    );
    *lock(&rig.verifier.gate) = Some(Gate::new());
    interrupt_in(&world, &rig, TurnState::Verifying, 0);
    assert_interrupted(&rig);
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn an_interrupt_that_comes_as_the_turn_finishes_does_not_change_how_it_ended() {
    let (world, rig) = Rig::one("int_finishing", [says("Done.")]);
    interrupt_in(&world, &rig, TurnState::Finishing, 0);
    let end = &rig.events.ends()[0];
    assert_eq!(
        end.outcome,
        TurnOutcome::Completed,
        "the turn had already ended"
    );
    assert_eq!(end.verification, Some(Verification::Skipped));
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn an_interrupt_that_comes_as_the_turn_finishes_still_holds_the_queue() {
    let more = Gate::new();
    let first = Response::new()
        .text("Done.")
        .wait(&more)
        .end(StopReason::End);
    let (world, rig) = Rig::one(
        "int_finishing_queue",
        [first, says("Now."), says("Queued.")],
    );
    rig.seen.interrupt_at(TurnState::Finishing, &rig.handle);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        rig.queue("second");
        more.open();
        rig.events.turns(1).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    });
    // The turn completed. The interrupt still means stop: the queued prompt
    // did not start by itself.
    assert_eq!(rig.events.ends().len(), 1);
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert_eq!(world.provider.requests().len(), 1);
    let state = rig.handle.state();
    assert_eq!((state.running_turn.clone(), state.queue_length), (None, 1));
    assert!(!rig.events.labels().contains(&"idle".to_owned()));

    // It follows the next prompt, as held prompts do.
    world.run(async {
        rig.prompt("now this");
        rig.events.seen("idle", 1).await;
    });
    assert_eq!(rig.events.ends().len(), 3);
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn an_answer_that_arrives_as_the_turn_is_interrupted_gives_the_call_one_result() {
    let world = World::new(
        "int_answer_in_flight",
        [calls(&[
            ("c1", "write", json!({"path": "a"})),
            // A call that goes on running after the interrupt keeps the
            // round open, so that every report for the first call is read.
            (
                "c2",
                "read",
                json!({"path": "b", "gate": "slow", "stubborn": true}),
            ),
        ])],
    );
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("write", Decision::Ask);
    rig.policy.wait_at(&world.gates);
    // The answer and the interrupt come in the same step: the task that
    // asked interrupts the turn and then returns its answer, so the answer
    // is on its way when the loop sees the interrupt and withdraws the
    // question.
    rig.policy.interrupt_as_it_answers(&rig.handle);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read b").await;
        rig.policy.questions(1).await;
        rig.events.seen("assistant", 1).await;
        world.gates.open("ask write");
        // The first call has its result, and the late answer has arrived.
        rig.events.seen("result:write:denied", 1).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        world.gates.open("slow");
        rig.events.turns(1).await;
    });
    assert_interrupted(&rig);
    let results = results(&rig);
    let outcomes: Vec<(&str, &ToolOutcome)> = results
        .iter()
        .map(|result| (result.call_id.as_str(), &result.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [("c1", &ToolOutcome::Denied), ("c2", &ToolOutcome::Success)],
        "one result each"
    );
    assert_eq!(world.log.count("start write a"), 0, "the write did not run");
    let shown = rig
        .events
        .labels()
        .iter()
        .filter(|label| label.starts_with("result:"))
        .count();
    assert_eq!(shown, 2, "and front ends saw each end once");
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn interrupting_twice_or_with_nothing_running_does_nothing() {
    let (world, rig) = Rig::one("int_idempotent", [Response::new().text("x").stall()]);
    assert!(!rig.handle.interrupt().interrupted, "nothing is running");
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        assert!(rig.handle.interrupt().interrupted);
        // The turn has not ended yet. A second interrupt is the same one.
        let again = rig.handle.interrupt();
        assert!(again.interrupted && again.undelivered.is_empty());
        rig.events.turns(1).await;
    });
    assert!(!rig.handle.interrupt().interrupted);
    assert_eq!(rig.events.ends().len(), 1);
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn a_retried_request_that_is_interrupted_leaves_nothing_of_either_attempt_half_done() {
    let response = Response::new()
        .tool_call("c1", "read", &json!({"path": "a"}))
        .event(retry(2))
        .event(start())
        .text("Second try")
        .stall();
    let (world, rig) = Rig::one("int_after_retry", [response]);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
    assert_interrupted(&rig);
    assert!(
        results(&rig).is_empty(),
        "the first attempt's read is not part of the message"
    );
    assert_the_next_request_is_accepted(&world, &rig);
}

#[test]
fn closing_a_session_interrupts_its_turn_and_ends_its_loop() {
    let (world, rig) = Rig::one("close", [Response::new().text("x").stall()]);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        rig.handle.close();
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Interrupted);
    let refused = rig
        .handle
        .prompt(vec![Part::text("hello?")], None)
        .unwrap_err();
    assert_eq!(refused.code, code::SESSION_NOT_OPEN);
}
