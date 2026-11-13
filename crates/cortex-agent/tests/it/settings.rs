//! Settings changed in a running session, and the second interrupt
//! (`docs/architecture/03-agent-loop.md`, "Several sessions at once";
//! `docs/architecture/02-runtime.md`, "Cancellation").
//!
//! A role's model, the session's level and its title are changed through
//! the handle. A change of model applies from the role's next request, and
//! a request that is in flight is accounted to the model it was sent to.

use std::sync::Arc;

use cortex_agent::{RoleModel, StatEvent};
use cortex_protocol::event::{Event, InfoNotice, Notice};
use cortex_protocol::item::ToolOutcome;
use cortex_protocol::lease::Level;
use cortex_protocol::model::ModelRole;
use cortex_protocol::{Item, LeaseId, ModelId, StopReason};
use cortex_testkit::{Response, ScriptedProvider, fixtures};
use serde_json::json;

use crate::{Rig, Setup, World, calls, says};

/// A second model, served by a provider of its own, so that a test sees
/// which of the two a request went to.
fn other_model(responses: impl IntoIterator<Item = Response>) -> (ScriptedProvider, RoleModel) {
    let provider = ScriptedProvider::new(responses);
    let model = RoleModel::new(Arc::new(provider.clone()), fixtures::plain_model());
    (provider, model)
}

fn main_model(rig: &Rig) -> ModelId {
    rig.handle.state().roles[&ModelRole::Main].model.clone()
}

/// The model each request was accounted to, in order.
fn request_models(rig: &Rig) -> Vec<String> {
    rig.stats
        .all()
        .into_iter()
        .filter_map(|event| match event {
            StatEvent::Request(request) => Some(request.model.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_change_of_the_main_model_applies_from_the_next_request() {
    let (world, rig) = Rig::one("set_model_next", [says("From the first model.")]);
    let (other, model) = other_model([says("From the second model.")]);
    world.run(async {
        rig.prompt("one");
        rig.events.turns(1).await;
    });
    assert_eq!(main_model(&rig).as_str(), fixtures::MODEL);

    rig.handle.set_role_model(ModelRole::Main, model);
    // The state says so at once, and front ends are told.
    assert_eq!(main_model(&rig).as_str(), fixtures::PLAIN_MODEL);
    assert!(
        matches!(
            rig.events.all().last().map(|event| &event.event),
            Some(Event::SessionUpdated(updated))
                if updated.session.roles[&ModelRole::Main].model.as_str() == fixtures::PLAIN_MODEL
        ),
        "front ends were not told of the change"
    );
    assert_eq!(
        rig.handle
            .role_model(&ModelRole::Main)
            .map(|model| model.entry.info.id.as_str().to_owned())
            .as_deref(),
        Some(fixtures::PLAIN_MODEL)
    );

    world.run(async {
        rig.prompt("two");
        rig.events.turns(2).await;
    });
    // The second turn went to the second model's client, as that model,
    // with the whole conversation.
    assert_eq!(world.provider.requests().len(), 1);
    let requests = other.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model.info.id.as_str(), fixtures::PLAIN_MODEL);
    assert_eq!(requests[0].messages.len(), 3, "the history went with it");
    assert_eq!(
        request_models(&rig),
        [fixtures::MODEL, fixtures::PLAIN_MODEL]
    );
}

#[test]
fn a_change_during_a_tool_round_applies_to_the_request_after_it() {
    let world = World::new(
        "set_model_mid_turn",
        [calls(&[(
            "c1",
            "read",
            json!({"path": "a", "gate": "slow"}),
        )])],
    );
    let rig = Setup::new(&world).start(&world);
    let (other, model) = other_model([says("Done, on the second model.")]);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        rig.handle.set_role_model(ModelRole::Main, model);
        world.gates.get("slow").open();
        rig.events.turns(1).await;
    });
    assert_eq!(world.provider.requests().len(), 1);
    assert_eq!(other.requests().len(), 1);
    // The request that asked for the tool is the first model's, and the
    // tool call is counted to it too; the one after the round is the
    // second's.
    assert_eq!(
        request_models(&rig),
        [fixtures::MODEL, fixtures::PLAIN_MODEL]
    );
    let last = rig
        .store
        .items()
        .into_iter()
        .rev()
        .find_map(|item| match item {
            Item::AssistantMessage(message) => Some(message),
            _ => None,
        })
        .expect("an assistant message");
    assert_eq!(last.stop_reason, Some(StopReason::End));
}

#[test]
fn a_request_in_flight_keeps_the_model_it_was_sent_to() {
    let world = World::new("set_model_in_flight", Vec::<Response>::new());
    let gate = world.gates.get("response");
    world.provider.push(
        Response::new()
            .text("Half")
            .wait(&gate)
            .text(" done.")
            .end(StopReason::End),
    );
    let rig = Setup::new(&world).start(&world);
    let (other, model) = other_model([says("unused")]);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        rig.handle.set_role_model(ModelRole::Main, model);
        gate.open();
        rig.events.turns(1).await;
    });
    // The response that was arriving is the first model's: nothing was
    // sent to the second, and the usage is counted to the first.
    assert_eq!(other.requests().len(), 0);
    assert_eq!(request_models(&rig), [fixtures::MODEL]);
    assert_eq!(main_model(&rig).as_str(), fixtures::PLAIN_MODEL);
}

#[test]
fn a_utility_role_can_be_given_a_model_while_the_session_runs() {
    let (_world, rig) = Rig::one("set_model_role", Vec::<Response>::new());
    assert!(rig.handle.role_model(&ModelRole::Fast).is_none());
    let (_other, model) = other_model([]);
    rig.handle.set_role_model(ModelRole::Fast, model);
    let state = rig.handle.state();
    assert_eq!(
        state.roles[&ModelRole::Fast].model.as_str(),
        fixtures::PLAIN_MODEL
    );
    assert_eq!(
        state.roles[&ModelRole::Main].model.as_str(),
        fixtures::MODEL
    );
}

#[test]
fn the_level_and_the_title_are_in_the_state_and_front_ends_are_told_once() {
    let (_world, rig) = Rig::one("set_level", Vec::<Response>::new());
    let updates = |rig: &Rig| {
        rig.events
            .all()
            .iter()
            .filter(|event| matches!(event.event, Event::SessionUpdated(_)))
            .count()
    };
    assert_eq!(rig.handle.state().level, Level::Supervised);
    let before = updates(&rig);

    rig.handle
        .set_level(Level::ReadOnly, Some(LeaseId::new("l_2")));
    let state = rig.handle.state();
    assert_eq!(state.level, Level::ReadOnly);
    assert_eq!(state.lease_id, Some(LeaseId::new("l_2")));
    assert_eq!(updates(&rig), before + 1);
    // The same again is no news.
    rig.handle
        .set_level(Level::ReadOnly, Some(LeaseId::new("l_2")));
    assert_eq!(updates(&rig), before + 1);

    rig.handle.set_title(Some("Fix the build".to_owned()));
    assert_eq!(rig.handle.state().title.as_deref(), Some("Fix the build"));
    assert_eq!(updates(&rig), before + 2);
    rig.handle.set_title(Some("Fix the build".to_owned()));
    assert_eq!(updates(&rig), before + 2);
}

#[test]
fn an_event_from_outside_the_loop_takes_the_next_seq_and_the_running_turn() {
    let world = World::new("emit_outside", [Response::new().text("x").stall()]);
    let rig = Setup::new(&world).start(&world);
    let notice = |text: &str| {
        Event::Notice(Notice::Info(InfoNotice {
            message: text.to_owned(),
        }))
    };
    rig.handle.emit(notice("before any turn"));
    world.run(async {
        let turn = rig.prompt("go").turn_id;
        rig.events.seen("delta:text", 1).await;
        rig.handle.emit(notice("during the turn"));
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;

        let events = rig.events.all();
        let found = |text: &str| {
            events
                .iter()
                .find(|event| {
                    matches!(&event.event, Event::Notice(Notice::Info(info)) if info.message == text)
                })
                .unwrap_or_else(|| panic!("no notice `{text}`"))
                .clone()
        };
        assert_eq!(found("before any turn").turn_id, None);
        assert_eq!(found("during the turn").turn_id, Some(turn));
    });
    // Every event has the seq after the one before it, ours included.
    let seqs: Vec<u64> = rig.events.all().iter().map(|event| event.seq).collect();
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(seqs, expected);
}

// ------------------------------------------------------------ "kill now"

fn outcomes(rig: &Rig) -> Vec<ToolOutcome> {
    rig.store
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::ToolResult(result) => Some(result.outcome),
            _ => None,
        })
        .collect()
}

/// A tool that stops in stages: asked to stop, it goes on until it is told
/// to stop now, as a command does that ignores the polite interrupt.
fn graceful_call() -> Response {
    calls(&[(
        "c1",
        "shell",
        json!({"id": "build", "gate": "never", "graceful": true}),
    )])
}

#[test]
fn a_second_interrupt_reaches_a_tool_that_is_stopping_in_stages() {
    let world = World::new("kill_second", [graceful_call()]);
    let rig = Setup::new(&world).start(&world);
    world.run(async {
        rig.prompt("go");
        world.log.has("start shell build").await;
        assert!(rig.handle.interrupt().interrupted);
        // The tool has been asked, and is sitting out its grace: the turn
        // is not over.
        world.log.has("asked shell build").await;
        crate::settle().await;
        assert!(
            rig.events.ends().is_empty(),
            "the turn ended on the first interrupt"
        );

        assert!(rig.handle.interrupt_now().interrupted);
        rig.events.turns(1).await;
    });
    assert_eq!(
        world.log.lines(),
        [
            "start shell build",
            "asked shell build",
            "killed shell build"
        ]
    );
    assert_eq!(outcomes(&rig), [ToolOutcome::Interrupted]);
}

#[test]
fn kill_now_alone_interrupts_the_turn_and_stops_the_tool_at_once() {
    let world = World::new("kill_first", [graceful_call()]);
    let rig = Setup::new(&world).start(&world);
    assert!(
        !rig.handle.interrupt_now().interrupted,
        "nothing is running"
    );
    world.run(async {
        rig.prompt("go");
        world.log.has("start shell build").await;
        assert!(rig.handle.interrupt_now().interrupted);
        rig.events.turns(1).await;
    });
    assert!(world.log.lines().contains(&"killed shell build".to_owned()));
    assert_eq!(outcomes(&rig), [ToolOutcome::Interrupted]);
    let end = &rig.events.ends()[0];
    assert_eq!(
        end.outcome,
        cortex_protocol::event::TurnOutcome::Interrupted
    );
}

#[test]
fn the_kill_of_one_turn_does_not_reach_the_next() {
    let world = World::new("kill_next_turn", [graceful_call(), graceful_call()]);
    let rig = Setup::new(&world).start(&world);
    world.run(async {
        rig.prompt("one");
        world.log.has("start shell build").await;
        let _ = rig.handle.interrupt_now();
        rig.events.turns(1).await;

        // A new turn has tokens of its own: an ordinary interrupt asks its
        // tool, and does not kill it.
        rig.prompt("two");
        world.log.has_times("start shell build", 2).await;
        let _ = rig.handle.interrupt();
        world.log.has_times("asked shell build", 1).await;
        crate::settle().await;
        assert_eq!(rig.events.ends().len(), 1, "the second turn was killed");
        let _ = rig.handle.interrupt_now();
        rig.events.turns(2).await;
    });
}
