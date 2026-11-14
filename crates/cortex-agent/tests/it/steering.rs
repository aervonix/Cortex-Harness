//! Steering, queued prompts and notices
//! (`docs/architecture/03-agent-loop.md`, "Steering and queued prompts").

use cortex_agent::{Ledger, TurnState};
use cortex_protocol::error::code;
use cortex_protocol::event::{Event, TurnOutcome};
use cortex_protocol::item::{Delivery, NoticeSource};
use cortex_protocol::lease::Budget;
use cortex_protocol::methods::Disposition;
use cortex_protocol::{ErrorKind, Item, Part, Role, StopReason};
use cortex_testkit::{Gate, Response};
use serde_json::json;

use crate::transitions::refused;
use crate::{Rig, Setup, World, assert_valid_history, calls, last_request, says, text_of};

/// The text of every user message in the log, in order.
fn prompts(rig: &Rig) -> Vec<String> {
    rig.store
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::UserMessage(user) => Some(
                user.parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

fn queue_lengths(rig: &Rig) -> Vec<u32> {
    rig.events
        .all()
        .iter()
        .filter_map(|event| match &event.event {
            Event::SessionUpdated(updated) => Some(updated.session.queue_length),
            _ => None,
        })
        .collect()
}

#[test]
fn a_steering_message_is_delivered_after_the_round_and_before_the_next_request() {
    let response = calls(&[("c1", "read", json!({"path": "a", "gate": "tool"}))]);
    let (world, rig) = Rig::one("steer_at_boundary", [response, says("Changed course.")]);
    world.run(async {
        let started = rig.prompt("go");
        world.log.has("start read a").await;
        let steered = rig.steer("use b instead");
        assert_eq!(steered.disposition, Disposition::Steering);
        assert_eq!(
            steered.turn_id, started.turn_id,
            "it belongs to the running turn"
        );
        world.gates.open("tool");
        rig.events.turns(1).await;
    });

    assert_eq!(rig.events.ends().len(), 1, "one turn");
    let request = last_request(&world.provider);
    let roles: Vec<&Role> = request
        .messages
        .iter()
        .map(|message| &message.role)
        .collect();
    assert_eq!(
        roles,
        [&Role::User, &Role::Assistant, &Role::Tool, &Role::User]
    );
    assert_eq!(text_of(&request.messages[3]), "use b instead");
    assert_valid_history(&request.messages);
    let Item::UserMessage(steer) = &rig.store.items()[3] else {
        panic!("the steering message is an item");
    };
    assert_eq!(steer.delivery, Some(Delivery::Steer));
}

#[test]
fn a_steering_message_that_finds_no_boundary_becomes_the_next_turn() {
    let more = Gate::new();
    let last = Response::new()
        .text("The answer.")
        .wait(&more)
        .end(StopReason::End);
    let (world, rig) = Rig::one("steer_becomes_turn", [last, says("And the rest.")]);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        // The model is writing its last answer: there is no round to wait
        // for.
        rig.steer("also this");
        rig.steer("and this");
        more.open();
        rig.events.seen("idle", 1).await;
    });

    let ends = rig.events.ends();
    assert_eq!(
        ends.len(),
        2,
        "the steering messages got a turn of their own"
    );
    let turns: Vec<String> = rig
        .events
        .all()
        .iter()
        .filter(|event| matches!(event.event, Event::TurnStarted(_)))
        .map(|event| event.turn_id.clone().unwrap().to_string())
        .collect();
    assert_eq!(turns, ["t_1", "t_2"]);
    // Both went into the one request of that turn, each as its own message.
    assert_eq!(prompts(&rig), ["go", "also this", "and this"]);
    assert_eq!(world.provider.requests().len(), 2);
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn queued_prompts_run_one_at_a_time_after_a_turn_that_completed() {
    let more = Gate::new();
    let first = Response::new()
        .text("One.")
        .wait(&more)
        .end(StopReason::End);
    let (world, rig) = Rig::one("queue_normal", [first, says("Two."), says("Three.")]);
    world.run(async {
        rig.prompt("first");
        rig.events.seen("delta:text", 1).await;
        let second = rig.queue("second");
        let third = rig.queue("third");
        assert_eq!(second.disposition, Disposition::Queued);
        assert_eq!(
            (second.turn_id.as_str(), third.turn_id.as_str()),
            ("t_2", "t_3"),
            "each is told the turn it will start"
        );
        assert_eq!(rig.handle.state().queue_length, 2);
        more.open();
        rig.events.seen("idle", 1).await;
    });

    assert_eq!(prompts(&rig), ["first", "second", "third"]);
    let outcomes: Vec<TurnOutcome> = rig
        .events
        .ends()
        .into_iter()
        .map(|end| end.outcome)
        .collect();
    assert_eq!(outcomes, vec![TurnOutcome::Completed; 3]);
    // The turns the queued prompts started are the ones they were told.
    let started: Vec<String> = rig
        .events
        .all()
        .iter()
        .filter(|event| matches!(event.event, Event::TurnStarted(_)))
        .map(|event| event.turn_id.clone().unwrap().to_string())
        .collect();
    assert_eq!(started, ["t_1", "t_2", "t_3"]);
    // Front ends were told the queue's length as it grew and shrank, and
    // that the session was idle once, at the end.
    assert_eq!(queue_lengths(&rig), [0, 1, 2, 1, 0, 0]);
    assert_eq!(
        rig.events
            .labels()
            .iter()
            .filter(|label| *label == "idle")
            .count(),
        1
    );
    assert_eq!(rig.handle.state().running_turn, None);
    let Item::UserMessage(queued) = &rig.store.items()[2] else {
        panic!("a user message");
    };
    assert_eq!(queued.delivery, Some(Delivery::Queue));
}

/// What a session looks like when two prompts are held behind a turn that
/// did not complete: nothing is running, nothing was sent for them, and the
/// session did not say it was idle.
fn assert_two_are_held(world: &World, rig: &Rig, requests_so_far: usize) {
    world.run(async {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    });
    assert_eq!(rig.handle.turn_state(), TurnState::Idle);
    let state = rig.handle.state();
    assert_eq!(state.running_turn, None);
    assert_eq!(state.queue_length, 2, "the prompts are kept");
    assert_eq!(
        *queue_lengths(rig).last().unwrap(),
        2,
        "and front ends know"
    );
    assert_eq!(
        world.provider.requests().len(),
        requests_so_far,
        "nothing ran by itself"
    );
    assert!(
        !rig.events.labels().contains(&"idle".to_owned()),
        "held is not idle"
    );
    assert_eq!(rig.events.ends().len(), 1);
}

/// The user's next prompt runs first, and the held prompts follow it in
/// order.
fn assert_the_held_follow_the_next_prompt(world: &World, rig: &Rig, before: &[&str]) {
    world.run(async {
        let next = rig.prompt("now this");
        assert_eq!(next.disposition, Disposition::Started);
        rig.events.seen("idle", 1).await;
    });
    let mut expected: Vec<&str> = before.to_vec();
    expected.extend(["now this", "second", "third"]);
    assert_eq!(prompts(rig), expected);
    let outcomes: Vec<TurnOutcome> = rig
        .events
        .ends()
        .into_iter()
        .map(|end| end.outcome)
        .collect();
    assert_eq!(outcomes[1..], vec![TurnOutcome::Completed; 3]);
    assert_eq!(rig.handle.state().queue_length, 0);
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn after_an_interrupt_queued_prompts_are_held_and_follow_the_next_prompt() {
    let stalls = Response::new().text("Thinking").stall();
    let (world, rig) = Rig::one(
        "queue_interrupt",
        [stalls, says("Now."), says("Two."), says("Three.")],
    );
    world.run(async {
        rig.prompt("first");
        rig.events.seen("delta:text", 1).await;
        rig.steer("a nudge");
        rig.queue("second");
        rig.queue("third");
        let interrupted = rig.handle.interrupt();
        assert!(interrupted.interrupted);
        // The steering message goes back to the editor. The queue stays.
        assert_eq!(interrupted.undelivered, [vec![Part::text("a nudge")]]);
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Interrupted);
    assert_two_are_held(&world, &rig, 1);
    assert_the_held_follow_the_next_prompt(&world, &rig, &["first"]);
}

#[test]
fn after_a_turn_that_failed_queued_prompts_are_held() {
    let more = Gate::new();
    let fails = Response::new().text("Half").wait(&more).error(refused());
    let (world, rig) = Rig::one(
        "queue_failed",
        [fails, says("Now."), says("Two."), says("Three.")],
    );
    world.run(async {
        rig.prompt("first");
        rig.events.seen("delta:text", 1).await;
        rig.queue("second");
        rig.queue("third");
        more.open();
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Failed);
    assert_two_are_held(&world, &rig, 1);
    assert_the_held_follow_the_next_prompt(&world, &rig, &["first"]);
}

#[test]
fn after_a_turn_that_hit_a_limit_queued_prompts_are_held() {
    let world = World::new(
        "queue_limit",
        [
            calls(&[("c1", "read", json!({"path": "a", "gate": "tool"}))]),
            says("Now."),
            says("Two."),
            says("Three."),
        ],
    );
    let mut setup = Setup::new(&world);
    // One request a turn: the request after the tool round is refused.
    setup.ledger = Some(Ledger::new(world.shared_clock()).with_turn_budget(Budget {
        requests: Some(1),
        ..Budget::default()
    }));
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("first");
        world.log.has("start read a").await;
        rig.queue("second");
        rig.queue("third");
        world.gates.open("tool");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Limit);
    assert_two_are_held(&world, &rig, 1);
    assert_the_held_follow_the_next_prompt(&world, &rig, &["first"]);
}

#[test]
fn a_steering_message_left_behind_by_a_failed_turn_is_held_with_the_queue() {
    let more = Gate::new();
    let fails = Response::new().text("Half").wait(&more).error(refused());
    let (world, rig) = Rig::one("steer_held", [fails, says("Now."), says("Nudged.")]);
    world.run(async {
        rig.prompt("first");
        rig.events.seen("delta:text", 1).await;
        rig.steer("a nudge");
        more.open();
        rig.events.turns(1).await;
        assert_eq!(
            rig.handle.state().queue_length,
            1,
            "kept, as a prompt to come"
        );
        rig.prompt("now this");
        rig.events.seen("idle", 1).await;
    });
    assert_eq!(prompts(&rig), ["first", "now this", "a nudge"]);
}

#[test]
fn a_prompt_during_a_turn_must_say_how_it_is_to_be_delivered() {
    let (world, rig) = Rig::one("delivery_required", [Response::new().text("x").stall()]);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;

        let refused = rig
            .handle
            .prompt(vec![Part::text("and?")], None)
            .unwrap_err();
        assert_eq!(
            (refused.kind.clone(), refused.code.as_str()),
            (ErrorKind::Usage, code::TURN_RUNNING)
        );
        let unknown = rig
            .handle
            .prompt(
                vec![Part::text("and?")],
                Some(Delivery::from_wire("whenever")),
            )
            .unwrap_err();
        assert_eq!(unknown.code, code::UNKNOWN_VALUE);
        assert!(unknown.message.contains("whenever"), "{}", unknown.message);

        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
    assert_eq!(prompts(&rig), ["go"], "neither was taken");
}

#[test]
fn an_edit_to_the_plan_reaches_the_model_as_a_notice_at_the_next_boundary() {
    let response = calls(&[("c1", "read", json!({"path": "a", "gate": "tool"}))]);
    let (world, rig) = Rig::one("plan_notice", [response, says("Noted."), says("Yes.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        rig.handle.notice(
            NoticeSource::Plan,
            "The user edited the plan: item 2 was skipped.",
        );
        world.gates.open("tool");
        rig.events.seen("idle", 1).await;
    });
    let labels = rig.events.labels();
    let note = rig.events.position("note:plan");
    assert!(
        rig.events.position("result:read:success") < note,
        "{labels:?}"
    );
    let request = last_request(&world.provider);
    assert!(text_of(request.messages.last().unwrap()).contains("item 2 was skipped"));
    assert_valid_history(&request.messages);

    // An edit made while the session is idle starts nothing, and is
    // delivered ahead of the next prompt.
    rig.handle
        .notice(NoticeSource::Plan, "The user added item 5.");
    world.run(async {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    });
    assert_eq!(rig.events.ends().len(), 1, "a notice starts no turn");
    world.run(async {
        rig.prompt("carry on");
        rig.events.seen("idle", 2).await;
    });
    let request = last_request(&world.provider);
    let tail: Vec<String> = request.messages[request.messages.len() - 2..]
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(
        tail,
        [
            format!("{}The user added item 5.", cortex_agent::NOTICE_LEAD),
            "carry on".to_owned()
        ]
    );
}
