//! The tool scheduler (`docs/architecture/03-agent-loop.md`, "Tool
//! rounds"): what runs together, what waits, the order results are
//! recorded in, and that every call gets exactly one.
//!
//! The fake tools write `start` and `end` lines to one log, and a call
//! waits at a gate the test opens. So "these ran together" is "both started
//! before either ended", and "this one waited" is "it started after the
//! other ended", read from the log when the round is over.

use cortex_agent::{Decision, Verdict};
use cortex_base::Rng;
use cortex_protocol::event::TurnOutcome;
use cortex_protocol::item::{ToolOutcome, ToolResultItem};
use cortex_protocol::{Item, Part};
use cortex_testkit::cases::{self, Outcome, ensure};
use serde_json::{Value, json};

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

fn position(log: &[String], line: &str) -> usize {
    log.iter()
        .position(|logged| logged == line)
        .unwrap_or_else(|| panic!("no `{line}` in {log:?}"))
}

/// Lets every task that can run without the test's help run.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn read_only_calls_run_together() {
    let response = calls(&[
        ("c1", "read", json!({"path": "a", "gate": "a"})),
        ("c2", "read", json!({"path": "b", "gate": "b"})),
        ("c3", "peek", json!({"path": "a", "gate": "c"})),
    ]);
    let (world, rig) = Rig::one("parallel_reads", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        // All three are running at once, each at its gate.
        world.log.has("start read a").await;
        world.log.has("start read b").await;
        world.log.has("start peek a").await;
        assert_eq!(
            world.log.lines().len(),
            3,
            "none has ended: {:?}",
            world.log.lines()
        );
        for gate in ["a", "b", "c"] {
            world.gates.open(gate);
        }
        rig.events.turns(1).await;
    });
    assert_eq!(results(&rig).len(), 3);
}

#[test]
fn calls_on_the_same_path_run_one_at_a_time_in_call_order() {
    let response = calls(&[
        (
            "c1",
            "write",
            json!({"path": "a", "id": "first", "gate": "first"}),
        ),
        ("c2", "write", json!({"path": "a", "id": "second"})),
        ("c3", "write", json!({"path": "a", "id": "third"})),
    ]);
    let (world, rig) = Rig::one("same_path", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start write first").await;
        // The second has every chance to start, and must not.
        settle().await;
        world.gates.open("first");
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        [
            "start write first",
            "end write first",
            "start write second",
            "end write second",
            "start write third",
            "end write third"
        ]
    );
}

#[test]
fn a_path_inside_another_is_the_same_key() {
    let response = calls(&[
        (
            "c1",
            "write",
            json!({"path": "dir", "id": "outer", "gate": "outer"}),
        ),
        ("c2", "write", json!({"path": "dir/a", "id": "inner"})),
    ]);
    let (world, rig) = Rig::one("nested_path", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start write outer").await;
        settle().await;
        world.gates.open("outer");
        rig.events.turns(1).await;
    });
    let log = world.log.lines();
    assert!(
        position(&log, "end write outer") < position(&log, "start write inner"),
        "{log:?}"
    );
}

#[test]
fn calls_on_unrelated_paths_run_together() {
    let response = calls(&[
        ("c1", "write", json!({"path": "a", "gate": "a"})),
        ("c2", "write", json!({"path": "b", "gate": "b"})),
    ]);
    let (world, rig) = Rig::one("unrelated_paths", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start write a").await;
        world.log.has("start write b").await;
        assert_eq!(world.log.lines().len(), 2);
        world.gates.open("b");
        world.gates.open("a");
        rig.events.turns(1).await;
    });
}

#[test]
fn an_exclusive_call_runs_alone() {
    let response = calls(&[
        ("c1", "read", json!({"path": "a", "gate": "a"})),
        ("c2", "shell", json!({"id": "build", "gate": "build"})),
        ("c3", "read", json!({"path": "b"})),
    ]);
    let (world, rig) = Rig::one("exclusive", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        // The command waits for the read before it, and the read after it
        // waits for the command.
        settle().await;
        assert_eq!(world.log.lines(), ["start read a"]);
        world.gates.open("a");
        world.log.has("start shell build").await;
        settle().await;
        assert_eq!(world.log.lines().len(), 3, "{:?}", world.log.lines());
        world.gates.open("build");
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        [
            "start read a",
            "end read a",
            "start shell build",
            "end shell build",
            "start read b",
            "end read b"
        ]
    );
}

#[test]
fn a_read_waits_for_the_write_before_it_to_what_it_reads() {
    let response = calls(&[
        ("c1", "write", json!({"path": "src/new", "gate": "write"})),
        ("c2", "read", json!({"path": "src/new"})),
        // A listing of the directory the file is in waits too.
        ("c3", "read", json!({"path": "src", "id": "list"})),
        // A read of something else does not.
        ("c4", "read", json!({"path": "other"})),
    ]);
    let (world, rig) = Rig::one("read_after_write", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start write src/new").await;
        world.log.has("end read other").await;
        settle().await;
        assert_eq!(world.log.count("start read src/new"), 0);
        assert_eq!(world.log.count("start read list"), 0);
        world.gates.open("write");
        rig.events.turns(1).await;
    });
    let log = world.log.lines();
    let written = position(&log, "end write src/new");
    assert!(written < position(&log, "start read src/new"), "{log:?}");
    assert!(written < position(&log, "start read list"), "{log:?}");
}

#[test]
fn results_are_recorded_in_call_order_whatever_order_they_finished_in() {
    let response = calls(&[
        ("c1", "read", json!({"path": "slow", "gate": "slow"})),
        ("c2", "read", json!({"path": "fast"})),
    ]);
    let (world, rig) = Rig::one("call_order", [response, says("Done.")]);
    world.run(async {
        rig.prompt("go");
        // The second finishes first, and front ends see it at once.
        rig.events.seen("result:read:success", 1).await;
        world.log.has("start read slow").await;
        world.gates.open("slow");
        rig.events.turns(1).await;
    });

    let shown: Vec<String> = rig
        .events
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::ToolResult(result) => Some(result.call_id.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(shown, ["c2", "c1"], "as they completed");
    let recorded: Vec<String> = results(&rig)
        .iter()
        .map(|result| result.call_id.to_string())
        .collect();
    assert_eq!(recorded, ["c1", "c2"], "in the model's order");
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn every_call_gets_exactly_one_result_however_it_ends() {
    let response = calls(&[
        ("c1", "panic", json!({"id": "boom"})),
        ("c2", "read", json!({"path": "a", "bad": 1})),
        ("c3", "shell", json!({"id": "rm"})),
        ("c4", "write", json!({"path": "a"})),
        ("c5", "write", json!({"path": "b", "id": "ok"})),
        ("c6", "nothing", json!({})),
        ("c7", "read", json!({"path": "c", "fail": true})),
    ]);
    let world = World::new("one_result_each", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for(
        "shell",
        Decision::Deny {
            reason: Some("Commands are off.".to_owned()),
        },
    );
    rig.policy.decide_for("write", Decision::Ask);
    // The first question is refused with a note; the second is allowed.
    rig.policy.answer_for(
        "write",
        Verdict::Deny {
            note: Some("Not that file.".to_owned()),
        },
    );
    rig.policy.answer_for("write", Verdict::Allow);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.policy.asked(), ["write a", "write ok"]);

    let results = results(&rig);
    let outcomes: Vec<(&str, &ToolOutcome)> = results
        .iter()
        .map(|result| (result.call_id.as_str(), &result.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("c1", &ToolOutcome::Error),
            ("c2", &ToolOutcome::Error),
            ("c3", &ToolOutcome::Denied),
            ("c4", &ToolOutcome::Denied),
            ("c5", &ToolOutcome::Success),
            ("c6", &ToolOutcome::Error),
            ("c7", &ToolOutcome::Error),
        ]
    );
    assert!(
        text(&results[0]).contains("panicked"),
        "{}",
        text(&results[0])
    );
    assert!(text(&results[2]).contains("Commands are off."));
    assert!(text(&results[3]).contains("Not that file."));
    assert_eq!(text(&results[6]), "read c failed");
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert_valid_history(&last_request(&world.provider).messages);

    // Front ends saw each call begin once and end once.
    let labels = rig.events.every_label();
    let count = |prefix: &str| {
        labels
            .iter()
            .filter(|label| label.starts_with(prefix))
            .count()
    };
    assert_eq!(
        (count("started:"), count("result:")),
        (7 + 2, 7),
        "{labels:?}"
    );
}

#[test]
fn no_more_than_the_limit_run_at_once() {
    let response = calls(&[
        ("c1", "read", json!({"path": "a", "gate": "a"})),
        ("c2", "read", json!({"path": "b", "gate": "b"})),
        ("c3", "read", json!({"path": "c", "gate": "c"})),
    ]);
    let world = World::new("max_parallel", [response, says("Done.")]);
    let mut setup = Setup::new(&world);
    setup.config.max_parallel_tools = 2;
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        world.log.has("start read b").await;
        settle().await;
        assert_eq!(world.log.count("start read c"), 0, "two is the limit");
        world.gates.open("a");
        world.log.has("start read c").await;
        world.gates.open("b");
        world.gates.open("c");
        rig.events.turns(1).await;
    });
    assert_eq!(results(&rig).len(), 3);
}

#[test]
fn questions_are_asked_one_at_a_time_in_call_order_while_allowed_calls_run() {
    let response = calls(&[
        ("c1", "write", json!({"path": "a"})),
        ("c2", "shell", json!({"id": "test"})),
        ("c3", "read", json!({"path": "unrelated"})),
    ]);
    let world = World::new("questions_in_order", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("write", Decision::Ask);
    rig.policy.decide_for("shell", Decision::Ask);
    rig.policy.wait_at(&world.gates);
    world.run(async {
        rig.prompt("go");
        rig.policy.questions(1).await;
        settle().await;
        assert_eq!(rig.policy.asked(), ["write a"], "one question is open");
        world.gates.open("ask write");
        rig.policy.questions(2).await;
        assert_eq!(rig.policy.asked(), ["write a", "shell test"]);
        world.gates.open("ask shell");
        rig.events.turns(1).await;
    });
    // The read after the command waits for it. The write did not wait for
    // the second question.
    assert_eq!(
        world.log.lines(),
        [
            "start write a",
            "end write a",
            "start shell test",
            "end shell test",
            "start read unrelated",
            "end read unrelated"
        ]
    );
}

#[test]
fn a_call_that_is_allowed_runs_while_a_question_for_a_later_call_is_open() {
    let response = calls(&[
        ("c1", "read", json!({"path": "a"})),
        ("c2", "write", json!({"path": "b"})),
    ]);
    let world = World::new("run_while_asking", [response, says("Done.")]);
    let rig = Setup::new(&world).start(&world);
    rig.policy.decide_for("write", Decision::Ask);
    rig.policy.wait_at(&world.gates);
    world.run(async {
        rig.prompt("go");
        rig.policy.questions(1).await;
        world.log.has("end read a").await;
        assert_eq!(
            world.log.count("start write b"),
            0,
            "not until it is answered"
        );
        world.gates.open("ask write");
        rig.events.turns(1).await;
    });
    assert_eq!(world.log.count("end write b"), 1);
}

// --------------------------------------------------------------- property

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Read,
    Write,
    Shell,
    Plain,
}

#[derive(Debug, Clone)]
struct Planned {
    sort: Sort,
    path: &'static str,
}

const PATHS: [&str; 4] = ["a", "b", "d", "d/x"];

fn related(first: &str, second: &str) -> bool {
    let inside = |inner: &str, outer: &str| {
        inner == outer
            || inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    inside(first, second) || inside(second, first)
}

/// The conflict rule, written again from the page and not from the code.
fn conflict(first: &Planned, second: &Planned) -> bool {
    match (first.sort, second.sort) {
        (Sort::Shell, _) | (_, Sort::Shell) => true,
        (Sort::Write, Sort::Write | Sort::Read) | (Sort::Read, Sort::Write) => {
            related(first.path, second.path)
        }
        _ => false,
    }
}

fn arguments(index: usize, call: &Planned) -> (String, &'static str, Value) {
    let id = format!("k{index}");
    let tool = match call.sort {
        Sort::Read => "read",
        Sort::Write => "write",
        Sort::Shell => "shell",
        Sort::Plain => "plain",
    };
    let arguments = match call.sort {
        Sort::Read | Sort::Write => json!({"path": call.path, "id": id}),
        Sort::Shell | Sort::Plain => json!({"id": id}),
    };
    (id, tool, arguments)
}

fn round_respects_the_rules(case: usize, round: &[Planned]) -> Outcome {
    let named: Vec<(String, &str, Value)> = round
        .iter()
        .enumerate()
        .map(|(index, call)| arguments(index, call))
        .collect();
    let scripted: Vec<(&str, &str, Value)> = named
        .iter()
        .map(|(id, tool, arguments)| (id.as_str(), *tool, arguments.clone()))
        .collect();
    let (world, rig) = Rig::one(
        &format!("property-{case}"),
        [calls(&scripted), says("Done.")],
    );
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });

    // One result each, in call order.
    let recorded: Vec<String> = results(&rig)
        .iter()
        .map(|result| result.call_id.to_string())
        .collect();
    let expected: Vec<String> = named.iter().map(|(id, _, _)| id.clone()).collect();
    ensure!(
        recorded == expected,
        "results {recorded:?}, calls {expected:?}"
    );

    // Every call ran once, and a call never overlapped an earlier one it
    // conflicts with.
    let log = world.log.lines();
    let at = |what: &str, index: usize| {
        let (id, tool, _) = &named[index];
        let line = format!("{what} {tool} {id}");
        let found: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, logged)| **logged == line)
            .map(|(position, _)| position)
            .collect();
        (found.len() == 1).then(|| found[0])
    };
    for later in 0..round.len() {
        let (Some(started), Some(_)) = (at("start", later), at("end", later)) else {
            return Err(format!("call {later} did not run exactly once: {log:?}").into());
        };
        for earlier in 0..later {
            if conflict(&round[earlier], &round[later]) {
                let ended = at("end", earlier).unwrap_or(usize::MAX);
                ensure!(
                    ended < started,
                    "call {later} started before call {earlier}, which it conflicts with, had \
                     ended: {log:?}"
                );
            }
        }
    }
    assert_valid_history(&last_request(&world.provider).messages);
    Ok(())
}

#[test]
fn any_round_runs_conflicting_calls_in_order_and_records_every_result() {
    let case = std::sync::atomic::AtomicUsize::new(0);
    cases::check(
        "any_round_runs_conflicting_calls_in_order_and_records_every_result",
        40,
        |rng| {
            let count = 1 + rng.below(6) as usize;
            (0..count)
                .map(|_| Planned {
                    sort: *cases::pick(
                        rng,
                        &[
                            Sort::Read,
                            Sort::Read,
                            Sort::Write,
                            Sort::Shell,
                            Sort::Plain,
                        ],
                    ),
                    path: cases::pick(rng, &PATHS),
                })
                .collect::<Vec<Planned>>()
        },
        |round| {
            let case = case.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            round_respects_the_rules(case, round)
        },
    );
}
