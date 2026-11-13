//! What the policy can say beyond allow, ask and deny
//! (`docs/architecture/03-agent-loop.md`, "Tool rounds"): that the turn
//! stops here, and that a call changes nothing.
//!
//! A denial is normally a result for the model, and the round goes on. A
//! `Stop` ends the turn with the outcome `denied`: the call it refused has
//! its denial, every call that had not started has a result that says it
//! did not run, and no further request is sent.

use cortex_agent::{Decision, Verdict};
use cortex_protocol::event::TurnOutcome;
use cortex_protocol::item::{ToolOutcome, ToolResultItem};
use cortex_protocol::{ErrorKind, Item, Part, StopReason};
use cortex_testkit::{Gate, Response};
use serde_json::json;

use crate::{Rig, Setup, World, assert_valid_history, calls, last_request, says};

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

fn text(result: &ToolResultItem) -> String {
    result
        .content
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

fn stop(reason: &str) -> Decision {
    Decision::Stop {
        reason: reason.to_owned(),
    }
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn a_stop_from_the_policy_ends_the_turn_denied_with_a_result_for_every_call() {
    let response = calls(&[
        ("c1", "read", json!({"path": "a"})),
        ("c2", "shell", json!({"id": "deploy"})),
        ("c3", "write", json!({"path": "b"})),
    ]);
    let world = World::new("stop_decided", [response, says("Next."), says("Unused.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy
        .decide_for("shell", stop("nobody can be asked, and the run stops at a denial"));
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });

    // The turn ended as `denied`, with a permission error that says why.
    let ends = rig.events.ends();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].outcome, TurnOutcome::Denied);
    let error = ends[0].error.as_ref().expect("a denied turn says why");
    assert_eq!(error.kind, ErrorKind::Permission);
    assert_eq!(error.code, "permission_denied");
    assert!(error.message.contains("nobody can be asked"), "{error:?}");
    // No request followed the round.
    assert_eq!(world.provider.requests().len(), 1);

    // Every call has exactly one result, in the model's order.
    let results = results(&rig);
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].outcome, ToolOutcome::Success, "the read ran");
    assert_eq!(results[1].outcome, ToolOutcome::Denied);
    assert!(text(&results[1]).contains("the turn was stopped"));
    assert!(text(&results[1]).contains("nobody can be asked"));
    assert_eq!(results[2].outcome, ToolOutcome::Interrupted);
    assert!(text(&results[2]).contains("did not run"), "{}", text(&results[2]));
    // Neither the refused command nor the write after it ran.
    assert_eq!(world.log.count("start shell deploy"), 0);
    assert_eq!(world.log.count("start write b"), 0);
    assert_eq!(world.log.count("end read a"), 1);

    // The history is whole: the next prompt is accepted as it is.
    world.run(async {
        rig.prompt("carry on");
        rig.events.turns(2).await;
    });
    assert_eq!(rig.events.ends()[1].outcome, TurnOutcome::Completed);
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn a_plain_denial_is_a_result_and_the_round_goes_on() {
    let response = calls(&[
        ("c1", "shell", json!({"id": "deploy"})),
        ("c2", "write", json!({"path": "b"})),
    ]);
    let world = World::new("deny_goes_on", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for(
        "shell",
        Decision::Deny {
            reason: Some("a rule denies it".to_owned()),
        },
    );
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert_eq!(world.provider.requests().len(), 2, "the model was told, and went on");
    let results = results(&rig);
    assert_eq!(results[0].outcome, ToolOutcome::Denied);
    assert_eq!(results[1].outcome, ToolOutcome::Success, "the write still ran");
}

#[test]
fn a_stop_as_the_answer_to_a_question_ends_the_turn_denied() {
    let response = calls(&[
        ("c1", "shell", json!({"id": "deploy"})),
        ("c2", "write", json!({"path": "b"})),
    ]);
    let world = World::new("stop_answered", [response, says("Unused.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("shell", Decision::Ask);
    rig.policy.answer_for(
        "shell",
        Verdict::Stop {
            reason: "no front end is attached to answer".to_owned(),
        },
    );
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Denied);
    assert_eq!(ends[0].error.as_ref().unwrap().kind, ErrorKind::Permission);
    assert_eq!(rig.policy.asked(), ["shell deploy"]);
    let results = results(&rig);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].outcome, ToolOutcome::Denied);
    assert!(text(&results[0]).contains("no front end is attached"));
    assert_eq!(results[1].outcome, ToolOutcome::Interrupted);
    assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
    assert_eq!(world.provider.requests().len(), 1);
}

#[test]
fn a_stop_withdraws_the_question_that_was_open() {
    let response = calls(&[
        ("c1", "write", json!({"path": "a"})),
        ("c2", "shell", json!({"id": "deploy"})),
    ]);
    let world = World::new("stop_withdraws", [response, says("Unused.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("write", Decision::Ask);
    rig.policy.decide_for("shell", stop("the session stops at a denial"));
    // The question about the write stays open: nobody opens its gate.
    rig.policy.wait_at(&world.gates);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Denied);
    let results = results(&rig);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].outcome, ToolOutcome::Interrupted, "never answered");
    assert_eq!(results[1].outcome, ToolOutcome::Denied);
    assert!(world.log.lines().is_empty(), "nothing ran");
    // The question was put, or was never put: either way none is left open
    // and none was answered.
    let asked = rig.policy.asked();
    assert!(asked.is_empty() || asked == ["write a"], "{asked:?}");
    assert_eq!(rig.policy.withdrawn(), asked);
}

#[test]
fn a_stop_while_the_response_is_arriving_lets_nothing_more_start() {
    let more = Gate::new();
    let response = Response::new()
        .tool_call("c1", "shell", &json!({"id": "deploy"}))
        .wait(&more)
        // Planned after the stop: a read would otherwise start at once.
        .tool_call("c2", "read", &json!({"path": "a"}))
        .tool_call("c3", "write", &json!({"path": "b"}))
        .end(StopReason::ToolCalls);
    let world = World::new("stop_streaming", [response, says("Unused.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("shell", stop("stop at the first denial"));
    world.run(async {
        rig.prompt("go");
        rig.policy.decisions(1).await;
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Denied);
    let results = results(&rig);
    let outcomes: Vec<&ToolOutcome> = results.iter().map(|result| &result.outcome).collect();
    assert_eq!(
        outcomes,
        [
            &ToolOutcome::Denied,
            &ToolOutcome::Interrupted,
            &ToolOutcome::Interrupted
        ]
    );
    assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
    // The assistant's message is recorded with its three calls, and each
    // has its result after it.
    let items = rig.store.items();
    assert!(matches!(items[1], Item::AssistantMessage(_)));
    assert_eq!(items.len(), 5);
}

#[test]
fn prompts_queued_behind_a_denied_turn_are_held() {
    let response = calls(&[("c1", "shell", json!({"id": "deploy"}))]);
    let world = World::new("stop_holds_queue", [response, says("Two."), says("Three.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("shell", Decision::Ask);
    rig.policy.answer_for(
        "shell",
        Verdict::Stop {
            reason: "stop at the first denial".to_owned(),
        },
    );
    rig.policy.wait_at(&world.gates);
    world.run(async {
        rig.prompt("go");
        rig.policy.questions(1).await;
        rig.queue("then commit");
        world.gates.open("ask shell");
        rig.events.turns(1).await;
        settle().await;
    });
    assert_eq!(rig.events.ends().len(), 1, "the queued prompt did not start");
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Denied);
    assert_eq!(rig.handle.state().queue_length, 1, "it is kept");
    assert_eq!(world.provider.requests().len(), 1);
}

#[test]
fn commands_the_policy_marks_read_only_run_together() {
    let response = calls(&[
        ("c1", "shell", json!({"id": "status", "gate": "one"})),
        ("c2", "shell", json!({"id": "log", "gate": "two"})),
        ("c3", "read", json!({"path": "a", "gate": "three"})),
    ]);
    let world = World::new("read_only_commands", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("shell", Decision::AllowReadOnly);
    world.run(async {
        rig.prompt("go");
        // All three are running at once: as planned, each command would
        // have run alone.
        world.log.has("start shell status").await;
        world.log.has("start shell log").await;
        world.log.has("start read a").await;
        assert_eq!(world.log.lines().len(), 3, "{:?}", world.log.lines());
        for gate in ["one", "two", "three"] {
            world.gates.open(gate);
        }
        rig.events.turns(1).await;
    });
    assert_eq!(results(&rig).len(), 3);
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
}

#[test]
fn a_command_marked_read_only_still_waits_for_the_write_before_it() {
    let response = calls(&[
        ("c1", "write", json!({"path": "a", "gate": "write"})),
        ("c2", "shell", json!({"id": "cat"})),
    ]);
    let world = World::new("read_only_after_write", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("shell", Decision::AllowReadOnly);
    world.run(async {
        rig.prompt("go");
        world.log.has("start write a").await;
        settle().await;
        assert_eq!(world.log.count("start shell cat"), 0, "it waits for the write");
        world.gates.open("write");
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        [
            "start write a",
            "end write a",
            "start shell cat",
            "end shell cat"
        ]
    );
}

#[test]
fn a_command_marked_read_only_does_not_start_before_the_response_ends() {
    let more = Gate::new();
    let response = Response::new()
        .tool_call("c1", "shell", &json!({"id": "status"}))
        .wait(&more)
        .text(" More words.")
        .end(StopReason::ToolCalls);
    let world = World::new("read_only_not_early", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("shell", Decision::AllowReadOnly);
    world.run(async {
        rig.prompt("go");
        rig.policy.decisions(1).await;
        // It is allowed and read-only, and it has an `exec` effect: a
        // command never runs for a response that may be thrown away.
        assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(world.log.count("end shell status"), 1);
}
