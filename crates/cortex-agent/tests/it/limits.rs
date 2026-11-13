//! Limits and loop guards (`docs/architecture/03-agent-loop.md`, "Loop
//! guards and limits"): the ledger charged before each request, shared
//! accounts, the repetition guard and the wall-clock limit.

use std::sync::Arc;
use std::time::Duration;

use cortex_agent::{Account, Ledger, RepetitionLimits};
use cortex_protocol::event::{LimitKind, Notice, TurnOutcome};
use cortex_protocol::item::NoticeSource;
use cortex_protocol::lease::Budget;
use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{ErrorKind, Item, StopReason};
use cortex_testkit::{Response, ScriptedProvider};
use serde_json::json;

use crate::{Rig, Setup, World, assert_valid_history, calls, last_request, says, text_of};

fn reads(id: &str) -> Response {
    calls(&[(id, "read", json!({"path": "a"}))])
}

fn notes(rig: &Rig, source: &NoticeSource) -> Vec<String> {
    rig.store
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::Notice(notice) if notice.source == *source => Some(notice.text),
            _ => None,
        })
        .collect()
}

#[test]
fn the_request_limit_of_a_turn_ends_it_with_a_notice_and_the_next_turn_starts_fresh() {
    let world = World::new(
        "request_limit",
        [reads("c1"), reads("c2"), reads("c3"), says("Done.")],
    );
    let mut setup = Setup::new(&world);
    setup.ledger = Some(Ledger::new(world.shared_clock()).with_turn_budget(Budget {
        requests: Some(2),
        ..Budget::default()
    }));
    // A model going round in circles is another test's business.
    setup.config.repetition = None;
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });

    assert_eq!(
        world.provider.requests().len(),
        2,
        "the third request was not sent"
    );
    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Limit);
    let error = ends[0].error.as_ref().unwrap();
    assert_eq!(
        (error.kind.clone(), error.code.as_str()),
        (ErrorKind::Limit, "request_limit")
    );
    assert_eq!(ends[0].usage.requests, 2);

    // Front ends get a notice, and the conversation gets a line that says
    // why it stopped.
    let notices = rig.events.notices();
    let [Notice::Limit(notice)] = notices.as_slice() else {
        panic!("{notices:#?}");
    };
    assert_eq!(
        (notice.limit.clone(), notice.reached),
        (LimitKind::Requests, true)
    );
    assert_eq!(
        notes(&rig, &NoticeSource::Limit),
        std::slice::from_ref(&notice.message)
    );

    // The round before the limit is whole, and a new turn has a budget of
    // its own.
    world.run(async {
        rig.prompt("carry on");
        rig.events.turns(2).await;
    });
    let ends = rig.events.ends();
    assert_eq!(
        ends[1].outcome,
        TurnOutcome::Completed,
        "two more requests were enough"
    );
    assert_eq!(world.provider.requests().len(), 4);
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn a_cost_limit_on_the_sessions_account_refuses_the_request_after_it_is_passed() {
    // 100,000 input tokens at $4 a million is 40 cents a request.
    let costly = |id: &str| {
        Response::new()
            .tool_call(id, "read", &json!({"path": id}))
            .tokens(TokenUsage {
                input: 100_000,
                ..TokenUsage::default()
            })
            .end(StopReason::ToolCalls)
    };
    let world = World::new("cost_limit", [costly("c1"), costly("c2"), costly("c3")]);
    let account = Account::new(
        "the session",
        Budget {
            usd_nanos: Some(700_000_000),
            ..Budget::default()
        },
        world.shared_clock(),
    );
    let mut setup = Setup::new(&world);
    setup.ledger = Some(Ledger::new(world.shared_clock()).with_account(account.clone()));
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });

    // The second request passed the cap while in flight. The third was
    // never sent.
    assert_eq!(world.provider.requests().len(), 2);
    assert_eq!(account.spent().usd_nanos, 800_000_000);
    let error = rig.events.ends()[0].error.clone().unwrap();
    assert_eq!(error.code, "cost_limit");
    assert!(error.message.contains("the session"), "{}", error.message);
}

#[test]
fn sessions_that_share_an_account_stop_together_at_its_cap() {
    let world = World::new("shared_account", []);
    let group = Account::new(
        "the group",
        Budget {
            requests: Some(3),
            ..Budget::default()
        },
        world.shared_clock(),
    );
    let session = |id: &str| {
        let mut setup = Setup::new(&world);
        setup.id = id.to_owned();
        setup.model.client = Arc::new(ScriptedProvider::new([
            says("One."),
            says("Two."),
            says("Three."),
        ]));
        setup.ledger = Some(Ledger::new(world.shared_clock()).with_account(group.clone()));
        setup.start(&world)
    };
    let first = session("s_a");
    let second = session("s_b");
    world.run(async {
        first.prompt("go");
        first.events.turns(1).await;
        second.prompt("go");
        second.events.turns(1).await;
        first.prompt("go");
        first.events.turns(2).await;
        // The group's three requests are spent, whoever asks next.
        second.prompt("go");
        second.events.turns(2).await;
        first.prompt("go");
        first.events.turns(3).await;
    });
    assert_eq!(group.spent().requests, 3);
    let outcome = |rig: &Rig, turn: usize| rig.events.ends()[turn].outcome.clone();
    assert_eq!(outcome(&first, 1), TurnOutcome::Completed);
    assert_eq!(outcome(&second, 1), TurnOutcome::Limit);
    assert_eq!(outcome(&first, 2), TurnOutcome::Limit);
    let error = second.events.ends()[1].error.clone().unwrap();
    assert!(error.message.contains("the group"), "{}", error.message);
}

#[test]
fn a_wall_clock_limit_is_checked_before_the_next_request() {
    let world = World::new(
        "time_limit",
        [
            calls(&[("c1", "read", json!({"path": "a", "gate": "tool"}))]),
            says("Done."),
        ],
    );
    let account = Account::new(
        "the run",
        Budget {
            wall_clock_ms: Some(60_000),
            ..Budget::default()
        },
        world.shared_clock(),
    );
    let mut setup = Setup::new(&world);
    setup.ledger = Some(Ledger::new(world.shared_clock()).with_account(account));
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        // The tool takes over a minute, on the fake clock.
        world.clock.advance(Duration::from_secs(61));
        world.gates.open("tool");
        rig.events.turns(1).await;
    });
    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Limit);
    assert_eq!(ends[0].error.as_ref().unwrap().code, "time_limit");
    // The round that was running is whole: its result is recorded.
    assert!(
        rig.store
            .items()
            .iter()
            .any(|item| matches!(item, Item::ToolResult(_)))
    );
    assert_eq!(world.provider.requests().len(), 1);
}

#[test]
fn the_same_round_three_times_is_told_to_the_model_and_five_times_stops_the_turn() {
    let rounds: Vec<Response> = (1..=6).map(|round| reads(&format!("c{round}"))).collect();
    let (world, rig) = Rig::one("repetition", rounds);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });

    assert_eq!(
        world.provider.requests().len(),
        5,
        "the sixth request was never sent"
    );
    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Limit);
    let error = ends[0].error.as_ref().unwrap();
    assert_eq!(
        (error.kind.clone(), error.code.as_str()),
        (ErrorKind::Limit, "repetition")
    );

    // The model was told once, after the third round, before its fourth
    // request.
    let warnings = notes(&rig, &NoticeSource::Repetition);
    assert_eq!(warnings.len(), 1);
    let requests = world.provider.requests();
    assert!(!format!("{:?}", requests[2].messages).contains("Repeating them"));
    assert!(
        text_of(requests[3].messages.last().unwrap()).contains("Repeating them changes nothing")
    );
    assert_valid_history(&requests[4].messages);
}

#[test]
fn a_round_that_differs_starts_the_count_again_and_the_guard_can_be_turned_off() {
    // Twice the same, then another path, then twice the same again: never
    // three in a row.
    let other = calls(&[("x", "read", json!({"path": "b"}))]);
    let (world, rig) = Rig::one(
        "repetition_broken",
        [
            reads("c1"),
            reads("c2"),
            other,
            reads("c4"),
            reads("c5"),
            says("Done."),
        ],
    );
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert!(notes(&rig, &NoticeSource::Repetition).is_empty());

    let mut rounds: Vec<Response> = (1..=7).map(|round| reads(&format!("c{round}"))).collect();
    rounds.push(says("Done."));
    let world = World::new("repetition_off", rounds);
    let mut setup = Setup::new(&world);
    setup.config.repetition = None;
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert_eq!(world.provider.requests().len(), 8);
}

#[test]
fn tool_calls_that_cannot_be_read_count_towards_the_guard() {
    let malformed = || {
        Response::new()
            .text("Trying.")
            .end(StopReason::MalformedToolCall)
    };
    let world = World::new(
        "repetition_malformed",
        [malformed(), malformed(), malformed()],
    );
    let mut setup = Setup::new(&world);
    setup.config.repetition = Some(RepetitionLimits {
        warn_at: 2,
        stop_at: 3,
    });
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    assert_eq!(
        rig.events.ends()[0].error.as_ref().unwrap().code,
        "repetition"
    );
    assert_eq!(notes(&rig, &NoticeSource::Repetition).len(), 1);
    assert_eq!(world.provider.requests().len(), 3);
}
