//! Calls start as soon as their arguments are complete, and a response
//! that is thrown away takes no side effect with it
//! (`docs/architecture/03-agent-loop.md`, "Streaming";
//! `docs/architecture/15-efficiency.md`, "Starting tools early").
//!
//! A scripted response waits at a gate after a tool call's block, so the
//! test can look at what has happened while the model "is still writing".

use cortex_agent::Decision;
use cortex_protocol::event::TurnOutcome;
use cortex_protocol::item::{ToolOutcome, ToolResultItem};
use cortex_protocol::{Item, Part, StopReason};
use cortex_testkit::{Gate, Response};
use cortex_tools::ReadTool;
use serde_json::json;

use crate::transitions::{refused, retry, start};
use crate::{Rig, Setup, World, assert_valid_history, last_request, says};

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

#[test]
fn a_read_begins_and_ends_before_the_response_has_ended() {
    let more = Gate::new();
    let response = Response::new()
        .text("Reading it.")
        .tool_call("c1", "read", &json!({"path": "a"}))
        .wait(&more)
        .text(" And then some more words.")
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("early_read", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        // The read has run to its end, and the model is still writing.
        rig.events.seen("result:read:success", 1).await;
        assert!(!rig.events.labels().contains(&"assistant".to_owned()));
        assert!(
            results(&rig).is_empty(),
            "nothing is recorded before the response ends"
        );
        more.open();
        rig.events.turns(1).await;
    });

    // The order front ends saw.
    let read_started = rig.events.position("started:read");
    let read_ended = rig.events.position("result:read:success");
    let response_ended = rig.events.position("assistant");
    assert!(read_started < read_ended && read_ended < response_ended);
    // The order of the log: the message, then its result.
    let items = rig.store.items();
    assert!(matches!(items[1], Item::AssistantMessage(_)));
    assert!(matches!(items[2], Item::ToolResult(_)));
    assert_eq!(world.log.count("start read a"), 1, "joined, not run again");
}

#[test]
fn a_call_with_side_effects_waits_for_the_response_to_end() {
    let more = Gate::new();
    let response = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .tool_call("c2", "shell", &json!({"id": "build"}))
        // Free of side effects, but after an exclusive call: it waits too.
        .tool_call("c3", "plain", &json!({"id": "marker"}))
        .wait(&more)
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("side_effects_wait", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        // All three blocks have ended and the response has not. (The test
        // after this one shows the same thing without relying on how long
        // planning takes.)
        rig.events.seen("delta:tool_args", 6).await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
        more.open();
        rig.events.turns(1).await;
    });
    assert!(rig.events.position("assistant") < rig.events.position("started:write"));
    assert_eq!(
        world.log.lines(),
        [
            "start write a",
            "end write a",
            "start shell build",
            "end shell build",
            "start plain marker",
            "end plain marker"
        ]
    );
}

#[test]
fn a_side_effecting_call_does_not_start_early_even_when_a_later_read_does() {
    let more = Gate::new();
    let response = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .tool_call("c2", "read", &json!({"path": "b"}))
        .wait(&more)
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("write_waits_read_runs", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        // The read is after the write and does not conflict with it. It
        // could only start once the write was planned, so the write had
        // its chance.
        world.log.has("end read b").await;
        assert_eq!(world.log.count("start write a"), 0);
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        ["start read b", "end read b", "start write a", "end write a"]
    );
}

#[test]
fn a_retried_response_that_asks_for_the_same_call_uses_the_early_result() {
    let fails = Gate::new();
    let response = Response::new()
        .text("Reading.")
        .tool_call("c1", "read", &json!({"path": "a"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .text("Reading again.")
        // The provider gives the call a new id the second time.
        .tool_call("c9", "read", &json!({"path": "a"}))
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("retry_reuses", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("result:read:success", 1).await;
        fails.open();
        rig.events.turns(1).await;
    });

    assert_eq!(world.log.count("start read a"), 1, "the tool ran once");
    let results = results(&rig);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].call_id.as_str(), "c9", "under the new call's id");
    assert_eq!(results[0].outcome, ToolOutcome::Success);
    assert_valid_history(&last_request(&world.provider).messages);

    // Front ends were told to clear the first attempt: its text and its
    // tool result. Then the result was shown again.
    let labels = rig.events.labels();
    assert_eq!(
        labels.iter().filter(|label| *label == "reset").count(),
        2,
        "{labels:?}"
    );
    assert_eq!(
        labels
            .iter()
            .filter(|label| *label == "result:read:success")
            .count(),
        2
    );
    // The log keeps the attempt, with the call it had asked for.
    let attempts = rig.store.attempts();
    assert_eq!(attempts.len(), 1);
    assert!(
        attempts[0]
            .partial
            .iter()
            .any(|part| matches!(part, Part::ToolCall(_)))
    );
}

#[test]
fn an_early_call_still_running_when_the_response_fails_is_joined_by_the_retry() {
    let fails = Gate::new();
    let response = Response::new()
        .tool_call("c1", "read", &json!({"path": "a", "gate": "slow"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .tool_call("c2", "read", &json!({"path": "a", "gate": "slow"}))
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("retry_joins_running", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        fails.open();
        // The retried response has asked for the call again, and it is
        // still the same run.
        rig.events.seen("started:read", 2).await;
        world.gates.open("slow");
        rig.events.turns(1).await;
    });
    assert_eq!(world.log.lines(), ["start read a", "end read a"]);
    assert_eq!(results(&rig)[0].call_id.as_str(), "c2");
}

/// A session whose `read` is the real tool, over real files.
fn with_real_read(world: &World) -> Rig {
    let mut setup = Setup::new(world);
    setup.toolbox = Some(world.toolbox().with(ReadTool::new()));
    let rig = setup.start(world);
    std::fs::write(
        world.dir.join("s_1").join("notes.txt"),
        "alpha\nbeta\ngamma\n",
    )
    .expect("write the file");
    rig
}

#[test]
fn an_early_result_the_retry_does_not_ask_for_is_discarded_and_forgotten() {
    let fails = Gate::new();
    let first = Response::new()
        .tool_call("c1", "read", &json!({"path": "notes.txt"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .text("On second thought, no need.")
        .end(StopReason::End);
    let second = Response::new()
        .tool_call("c2", "read", &json!({"path": "notes.txt"}))
        .end(StopReason::ToolCalls);
    let world = World::new("retry_discards", [first, second, says("Done.")]);
    let rig = with_real_read(&world);
    let file = rig.workspace.paths.resolve("notes.txt").unwrap();
    world.run(async {
        rig.prompt("go");
        rig.events.seen("result:read:success", 1).await;
        assert!(
            rig.workspace.files.shown(file.abs()).is_some(),
            "the read recorded what it showed"
        );
        fails.open();
        rig.events.turns(1).await;
    });

    // The model never saw that result: it is not in the log, and the table
    // of seen files no longer says the file was shown.
    assert!(results(&rig).is_empty());
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert!(rig.workspace.files.shown(file.abs()).is_none());
    assert!(rig.workspace.files.snapshot(file.abs()).is_none());

    // So when the model does read the file, it gets the content, not
    // "unchanged since you read it".
    world.run(async {
        rig.prompt("read it after all");
        rig.events.turns(2).await;
    });
    let results = results(&rig);
    assert_eq!(results.len(), 1);
    let shown = text(&results[0]);
    assert!(shown.contains("alpha\nbeta\ngamma"), "{shown}");
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn a_retry_that_reads_the_same_file_another_way_does_not_meet_the_discarded_read() {
    let fails = Gate::new();
    let response = Response::new()
        .tool_call("c1", "read", &json!({"path": "notes.txt"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        // The same file, other arguments: not the same call.
        .tool_call(
            "c2",
            "read",
            &json!({"path": "notes.txt", "offset": 2, "limit": 1}),
        )
        .end(StopReason::ToolCalls);
    let world = World::new("retry_other_arguments", [response, says("Done.")]);
    let rig = with_real_read(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("result:read:success", 1).await;
        fails.open();
        rig.events.turns(1).await;
    });
    let results = results(&rig);
    assert_eq!(results.len(), 1);
    let shown = text(&results[0]);
    // Had the discarded read still been in the table, this would say the
    // lines were already shown and are unchanged.
    assert!(shown.contains("beta"), "{shown}");
    assert!(!shown.contains("unchanged"), "{shown}");
}

#[test]
fn no_side_effect_runs_for_a_response_that_fails_or_is_retried() {
    // The response fails for good after a write was asked for.
    let failing = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .tool_call("c2", "read", &json!({"path": "b"}))
        .error(refused());
    let (world, rig) = Rig::one("no_side_effect_failed", [failing]);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Failed);
    assert_eq!(world.log.count("start write a"), 0);
    assert!(
        results(&rig).is_empty(),
        "and nothing of the round is recorded"
    );

    // The response is retried and no longer asks for the write.
    let retried = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .tool_call("c2", "shell", &json!({"id": "rm"}))
        .event(retry(2))
        .event(start())
        .text("Nothing to change.")
        .end(StopReason::End);
    let (world, rig) = Rig::one("no_side_effect_retried", [retried]);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
    assert!(results(&rig).is_empty());
}

#[test]
fn a_question_is_put_while_the_model_is_still_writing_and_withdrawn_if_the_response_fails() {
    let fails = Gate::new();
    let response = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .text("No change needed.")
        .end(StopReason::End);
    let world = World::new("question_withdrawn", [response]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("write", Decision::Ask);
    rig.policy.wait_at(&world.gates);
    world.run(async {
        rig.prompt("go");
        // The question is open and the response has not ended.
        rig.policy.questions(1).await;
        assert!(!rig.events.labels().contains(&"assistant".to_owned()));
        fails.open();
        rig.events.turns(1).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    });
    assert_eq!(rig.policy.asked(), ["write a"]);
    assert_eq!(rig.policy.withdrawn(), ["write a"], "nobody is left asking");
    assert!(world.log.lines().is_empty());
}

#[test]
fn a_call_approved_while_the_model_writes_still_waits_for_the_end_if_it_has_side_effects() {
    let more = Gate::new();
    let response = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .wait(&more)
        .end(StopReason::ToolCalls);
    let world = World::new("approved_early", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("write", Decision::Ask);
    world.run(async {
        rig.prompt("go");
        // Asked and answered (allowed) during the stream.
        rig.policy.questions(1).await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(
            world.log.lines().is_empty(),
            "approved, and still waiting for the end"
        );
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(world.log.lines(), ["start write a", "end write a"]);
}
