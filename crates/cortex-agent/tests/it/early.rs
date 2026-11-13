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
        // All three calls have been planned and shown to the policy, so
        // each has had its chance to start, and the response has not ended.
        rig.policy.decisions(3).await;
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

/// `plan` and `remember` touch nothing outside the session: no effect, and
/// scheduled as read-only. What keeps them from starting while the model
/// is still writing is the marker on their plan.
#[test]
fn a_call_that_changes_the_session_waits_for_the_response_to_end() {
    let more = Gate::new();
    let response = Response::new()
        .tool_call("c1", "plain", &json!({"id": "plan", "session": true}))
        // The same tool without the marker: free to start at once.
        .tool_call("c2", "plain", &json!({"id": "look"}))
        .wait(&more)
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("session_change_waits", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        // The unmarked call is after the marked one, so it could only
        // start once the marked one was planned and had its chance.
        world.log.has("end plain look").await;
        assert_eq!(world.log.count("start plain plan"), 0);
        assert!(!rig.events.labels().contains(&"assistant".to_owned()));
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        [
            "start plain look",
            "end plain look",
            "start plain plan",
            "end plain plan"
        ]
    );
}

#[test]
fn a_session_change_is_not_made_for_a_response_that_is_thrown_away() {
    // The response fails for good after the call was planned.
    let fails = Gate::new();
    let failing = Response::new()
        .tool_call("c1", "plain", &json!({"id": "plan", "session": true}))
        .tool_call("c2", "plain", &json!({"id": "look"}))
        .wait(&fails)
        .error(refused());
    let (world, rig) = Rig::one("session_change_failed", [failing]);
    world.run(async {
        rig.prompt("go");
        world.log.has("end plain look").await;
        fails.open();
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Failed);
    assert_eq!(world.log.count("start plain plan"), 0);

    // The response is retried and no longer asks for it.
    let fails = Gate::new();
    let retried = Response::new()
        .tool_call("c1", "plain", &json!({"id": "plan", "session": true}))
        .tool_call("c2", "plain", &json!({"id": "look"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .text("No plan needed.")
        .end(StopReason::End);
    let (world, rig) = Rig::one("session_change_retried", [retried]);
    world.run(async {
        rig.prompt("go");
        world.log.has("end plain look").await;
        fails.open();
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert_eq!(world.log.count("start plain plan"), 0);
}

/// Two calls that change the session run one at a time, in the model's
/// order: of two `plan` calls the second is the one that stands.
#[test]
fn two_calls_that_change_the_session_run_in_the_models_order() {
    let response = Response::new()
        .tool_call(
            "c1",
            "plain",
            &json!({"id": "first", "session": true, "gate": "first"}),
        )
        .tool_call("c2", "plain", &json!({"id": "second", "session": true}))
        // Unmarked, and after both: it runs beside the first.
        .tool_call("c3", "plain", &json!({"id": "look"}))
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("session_changes_in_order", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start plain first").await;
        world.log.has("end plain look").await;
        assert_eq!(
            world.log.count("start plain second"),
            0,
            "the second waits for the first"
        );
        world.gates.open("first");
        rig.events.turns(1).await;
    });
    let lines = world.log.lines();
    let at = |line: &str| lines.iter().position(|found| found == line).unwrap();
    assert!(
        at("end plain first") < at("start plain second"),
        "{lines:?}"
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
    let more = Gate::new();
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
        // The response does not end until the second read has run, so it
        // runs while the first read's result is still being kept.
        .wait(&more)
        .end(StopReason::ToolCalls);
    let world = World::new("retry_other_arguments", [response, says("Done.")]);
    let rig = with_real_read(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("result:read:success", 1).await;
        fails.open();
        rig.events.seen("result:read:success", 2).await;
        more.open();
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
    // The response fails for good after a write and a command were asked
    // for, and after both were planned and could have started.
    let fails = Gate::new();
    let failing = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .tool_call("c2", "shell", &json!({"id": "rm"}))
        .tool_call("c3", "read", &json!({"path": "b"}))
        .wait(&fails)
        .error(refused());
    let (world, rig) = Rig::one("no_side_effect_failed", [failing]);
    world.run(async {
        rig.prompt("go");
        rig.policy.decisions(3).await;
        fails.open();
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Failed);
    assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
    assert!(
        results(&rig).is_empty(),
        "and nothing of the round is recorded"
    );

    // The response is retried and no longer asks for the write.
    let fails = Gate::new();
    let retried = Response::new()
        .tool_call("c1", "write", &json!({"path": "a"}))
        .tool_call("c2", "shell", &json!({"id": "rm"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .text("Nothing to change.")
        .end(StopReason::End);
    let (world, rig) = Rig::one("no_side_effect_retried", [retried]);
    world.run(async {
        rig.prompt("go");
        rig.policy.decisions(2).await;
        fails.open();
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

#[test]
fn a_tool_that_declares_no_effect_but_runs_alone_does_not_start_early() {
    // What a tool from outside looks like when its author said nothing
    // about what it does: no effect, scheduled to run alone.
    let more = Gate::new();
    let response = Response::new()
        .tool_call("c1", "solo", &json!({"id": "unknown"}))
        .wait(&more)
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("solo_waits", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        rig.policy.decisions(1).await;
        assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        ["start solo unknown", "end solo unknown"]
    );
    assert!(rig.events.position("assistant") < rig.events.position("started:solo"));
}

#[test]
fn a_kept_read_is_not_used_ahead_of_a_write_the_retried_response_puts_before_it() {
    let fails = Gate::new();
    let response = Response::new()
        .tool_call("c1", "read", &json!({"path": "a"}))
        .wait(&fails)
        .event(retry(2))
        .event(start())
        // The second time the model writes the file first, then reads it.
        .tool_call("c2", "write", &json!({"path": "a"}))
        .tool_call("c3", "read", &json!({"path": "a"}))
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("kept_read_after_write", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("result:read:success", 1).await;
        fails.open();
        rig.events.turns(1).await;
    });
    // The read that ran for the first response saw the file before the
    // write. The read the model now asks for comes after it.
    assert_eq!(
        world.log.lines(),
        [
            "start read a",
            "end read a",
            "start write a",
            "end write a",
            "start read a",
            "end read a"
        ]
    );
    let order: Vec<String> = results(&rig)
        .iter()
        .map(|result| result.call_id.to_string())
        .collect();
    assert_eq!(order, ["c2", "c3"]);
    assert_valid_history(&last_request(&world.provider).messages);
}

/// The file the fake `read` says it showed, when it is told to `record`.
fn recorded(rig: &Rig, path: &str) -> bool {
    let file = rig.workspace.paths.resolve(path).unwrap();
    rig.workspace.files.shown(file.abs()).is_some()
}

#[test]
fn a_discarded_read_that_was_still_running_is_forgotten_when_it_ends() {
    let fails = Gate::new();
    let response = Response::new()
        // It ignores its cancellation, and notes what it "showed" only at
        // its end: the worst case for a run that is thrown away.
        .tool_call(
            "c1",
            "read",
            &json!({"path": "a", "gate": "slow", "stubborn": true, "record": true}),
        )
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .text("No need to read it.")
        .end(StopReason::End);
    let (world, rig) = Rig::one("discard_running", [response]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        fails.open();
        // The response that counts has ended, and the read is still going.
        rig.events.seen("assistant", 1).await;
        assert!(rig.events.ends().is_empty(), "the turn waits for it");
        world.gates.open("slow");
        rig.events.turns(1).await;
    });
    assert_eq!(world.log.lines(), ["start read a", "end read a"]);
    assert!(results(&rig).is_empty());
    assert!(
        !recorded(&rig, "a"),
        "what it noted at its end was forgotten"
    );
}

#[test]
fn a_call_waits_for_a_discarded_read_of_the_same_file_to_end() {
    let fails = Gate::new();
    let more = Gate::new();
    let response = Response::new()
        .tool_call(
            "c1",
            "read",
            &json!({"path": "a", "gate": "slow", "stubborn": true, "record": true}),
        )
        .wait(&fails)
        .event(retry(2))
        .event(start())
        // The same file, read another way: the first read is discarded,
        // and is still running.
        .tool_call(
            "c2",
            "read",
            &json!({"path": "a", "id": "second", "record": true}),
        )
        .wait(&more)
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("wait_for_discarded", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        fails.open();
        // The second read has been planned, and could start but for the
        // first, which may still write to the table of seen files.
        rig.policy.decisions(2).await;
        assert_eq!(world.log.lines(), ["start read a"]);
        world.gates.open("slow");
        world.log.has("end read second").await;
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        [
            "start read a",
            "end read a",
            "start read second",
            "end read second"
        ]
    );
    // The table holds what the second read noted, not what the first did
    // and then lost: it was forgotten before the second began.
    assert!(recorded(&rig, "a"));
    assert_eq!(results(&rig).len(), 1);
}

#[test]
fn a_call_whose_block_ends_before_an_earlier_calls_block_waits_for_it() {
    use cortex_llm::ModelEvent;
    use cortex_protocol::RawJson;
    let more = Gate::new();
    let raw = || RawJson::new("{}");
    // The write is block 0 and the read block 1, and the read's block ends
    // first.
    let response = Response::new()
        .event(ModelEvent::ToolCallStart {
            index: 0,
            id: "c1".to_owned(),
            name: "write".to_owned(),
        })
        .event(ModelEvent::ToolCallStart {
            index: 1,
            id: "c2".to_owned(),
            name: "read".to_owned(),
        })
        .event(ModelEvent::ToolArgsDelta {
            index: 1,
            json: "{\"path\":\"a\"}".to_owned(),
        })
        .event(ModelEvent::BlockEnd {
            index: 1,
            raw: raw(),
        })
        .wait(&more)
        .event(ModelEvent::ToolArgsDelta {
            index: 0,
            json: "{\"path\":\"a\"}".to_owned(),
        })
        .event(ModelEvent::BlockEnd {
            index: 0,
            raw: raw(),
        })
        .end(StopReason::ToolCalls);
    let (world, rig) = Rig::one("blocks_out_of_order", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        // The read is planned and free of side effects, but a call before
        // it is still being written.
        rig.policy.decisions(1).await;
        assert!(world.log.lines().is_empty(), "{:?}", world.log.lines());
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        ["start write a", "end write a", "start read a", "end read a"]
    );
    let order: Vec<String> = results(&rig)
        .iter()
        .map(|result| result.call_id.to_string())
        .collect();
    assert_eq!(
        order,
        ["c1", "c2"],
        "the model's order, not the order the blocks ended in"
    );
    assert_valid_history(&last_request(&world.provider).messages);
}
