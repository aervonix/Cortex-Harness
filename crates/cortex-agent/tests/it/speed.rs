//! Speed tiers in the loop (`docs/decisions/0027-speed-tiers.md`): the
//! speed each request asks for, the notices for a fallback and a cooldown,
//! and the session state's speed in effect.
//!
//! The scripted provider stands in for the client, so a test plays the
//! client's part by hand: it puts a cooldown in the shared table and
//! scripts the events the client would have sent.

use std::time::Duration;

use cortex_agent::RoleModel;
use cortex_base::UtcTime;
use cortex_llm::{CooldownChange, ModelEvent};
use cortex_protocol::event::{Event, Notice};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::RoleState;
use cortex_protocol::speed::{CooldownState, SpeedFallbackBy, SpeedFallbackReason};
use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{Item, ModelId, ProviderId, Speed, StopReason};
use cortex_testkit::{Gate, Response, fixtures};
use serde_json::json;

use crate::transitions::{fell_back, start};
use crate::{Rig, Setup, World, calls, says};

const UNTIL: &str = "2026-11-13T10:10:00Z";

fn fast_session(world: &World) -> Rig {
    let mut setup = Setup::new(world);
    setup.speeds.session = Speed::Fast;
    setup.start(world)
}

fn main_role(rig: &Rig) -> RoleState {
    rig.handle.state().roles[&ModelRole::Main].clone()
}

fn fallbacks(rig: &Rig) -> Vec<cortex_protocol::speed::SpeedFallback> {
    rig.events
        .notices()
        .into_iter()
        .filter_map(|notice| match notice {
            Notice::SpeedFallback(fallback) => Some(fallback),
            _ => None,
        })
        .collect()
}

/// The main role's state as front ends were last told it.
fn told(rig: &Rig) -> RoleState {
    rig.events
        .all()
        .iter()
        .rev()
        .find_map(|event| match &event.event {
            Event::SessionUpdated(updated) => Some(updated.session.roles[&ModelRole::Main].clone()),
            _ => None,
        })
        .expect("a session.updated event")
}

fn cooldown(state: CooldownState, until: Option<&str>) -> ModelEvent {
    ModelEvent::SpeedCooldown(CooldownChange {
        speed: Speed::Fast,
        state,
        until: until.map(|time| UtcTime::parse(time).unwrap()),
    })
}

fn turn(world: &World, rig: &Rig, turns: usize) {
    world.run(async {
        rig.prompt("go");
        rig.events.turns(turns).await;
    });
}

#[test]
fn each_request_asks_for_the_speed_its_role_is_set_to_and_a_change_applies_from_the_next() {
    let world = World::new(
        "speed_setting",
        [
            calls(&[("c1", "read", json!({"path": "a", "gate": "tool"}))]),
            says("Done."),
            says("Again."),
        ],
    );
    let rig = fast_session(&world);
    assert_eq!(main_role(&rig).speed, Speed::Fast);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        // Changed while a request's tool round is running.
        rig.handle.set_speed(Speed::Standard);
        world.gates.open("tool");
        rig.events.turns(1).await;

        // A role's own speed wins over the session's.
        rig.handle.set_role_speed(ModelRole::Main, Speed::Fast);
        rig.prompt("again");
        rig.events.turns(2).await;
    });
    let asked: Vec<Speed> = world
        .provider
        .requests()
        .into_iter()
        .map(|request| request.speed)
        .collect();
    assert_eq!(asked, [Speed::Fast, Speed::Standard, Speed::Fast]);
    let state = rig.handle.state();
    assert_eq!(state.speed, Speed::Standard, "the session's setting");
    assert_eq!(
        state.roles[&ModelRole::Main].speed,
        Speed::Fast,
        "the role's own"
    );
    assert_eq!(
        told(&rig).speed,
        Speed::Fast,
        "front ends were told of each change"
    );
}

#[test]
fn a_utility_role_does_not_follow_the_sessions_speed() {
    let world = World::new("speed_utility", []);
    let mut setup = Setup::new(&world);
    setup.speeds.session = Speed::Fast;
    setup.utility = Some(RoleModel::new(
        setup.model.client.clone(),
        fixtures::model(),
    ));
    let rig = setup.start(&world);
    let roles = rig.handle.state().roles;
    assert_eq!(roles[&ModelRole::Main].speed, Speed::Fast);
    assert_eq!(roles[&ModelRole::Fast].speed, Speed::Standard);
    assert_eq!(roles[&ModelRole::Fast].speed_in_effect, Speed::Standard);

    rig.handle.set_role_speed(ModelRole::Fast, Speed::Fast);
    assert_eq!(
        rig.handle.state().roles[&ModelRole::Fast].speed_in_effect,
        Speed::Fast
    );
}

#[test]
fn a_request_the_provider_served_at_another_speed_is_recorded_and_told_when_it_is_news() {
    let tokens = TokenUsage {
        input: 1_000,
        ..TokenUsage::default()
    };
    let downgraded = || {
        Response::new()
            .served_at(Speed::Standard)
            .tokens(tokens.clone())
            .text("Done.")
            .end(StopReason::End)
    };
    let served = Response::new()
        .tokens(tokens.clone())
        .text("Done.")
        .end(StopReason::End);
    let world = World::new(
        "speed_downgrade",
        [downgraded(), downgraded(), served, downgraded()],
    );
    let rig = fast_session(&world);
    for turns in 1..=4 {
        turn(&world, &rig, turns);
    }

    // Asked for fast, served standard: on the record, and priced at the
    // standard rate ($4 a million, not $8).
    let usages: Vec<_> = rig
        .store
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::AssistantMessage(assistant) => assistant.usage,
            _ => None,
        })
        .collect();
    assert_eq!(usages[0].speed, Speed::Standard);
    assert_eq!(usages[0].speed_requested, Some(Speed::Fast));
    assert_eq!(usages[0].cost.as_ref().unwrap().usd_nanos, 4_000_000);
    assert_eq!(
        (usages[2].speed.clone(), usages[2].speed_requested.clone()),
        (Speed::Fast, None)
    );
    assert_eq!(usages[2].cost.as_ref().unwrap().usd_nanos, 8_000_000);

    // Four requests, three of them downgraded: two notices. The second
    // downgrade in a row is not news; the one after a request that ran at
    // fast is.
    let notices = fallbacks(&rig);
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0].by, SpeedFallbackBy::Provider);
    assert_eq!(notices[0].reason, SpeedFallbackReason::Unspecified);
    assert_eq!(
        (notices[0].requested.clone(), notices[0].served.clone()),
        (Speed::Fast, Speed::Standard)
    );
    assert_eq!(notices[0].role, ModelRole::Main);
    assert_eq!(notices[0].cooldown_until, None);
    assert!(
        notices[0].message.contains("billed at the standard rate"),
        "{}",
        notices[0].message
    );
    // The session's usage keeps the two speeds apart.
    let by_speed: Vec<(Speed, u64)> = rig
        .handle
        .usage()
        .by_model
        .into_iter()
        .map(|row| (row.speed, row.totals.requests))
        .collect();
    assert_eq!(by_speed, [(Speed::Standard, 3), (Speed::Fast, 1)]);
}

#[test]
fn a_fallback_and_its_cooldown_are_notices_and_the_state_shows_the_speed_in_effect() {
    let refused = Gate::new();
    // The tier refuses the first request: the client falls back, starts a
    // cooldown and sends the request at standard.
    let first = Response::events([])
        .wait(&refused)
        .event(fell_back(SpeedFallbackReason::RateLimited, Some(UNTIL)))
        .event(cooldown(CooldownState::Started, Some(UNTIL)))
        .event(start())
        .served_at(Speed::Standard)
        .text("At standard.")
        .end(StopReason::End);
    // While it cools, every request says it was sent at standard.
    let during = || {
        Response::events([
            fell_back(SpeedFallbackReason::Cooldown, Some(UNTIL)),
            start(),
        ])
        .served_at(Speed::Standard)
        .text("Still at standard.")
        .end(StopReason::End)
    };
    // The first request after the time has passed asks for the tier again.
    let after = Response::events([cooldown(CooldownState::Ended, None), start()])
        .text("Fast again.")
        .end(StopReason::End);
    let world = World::new("speed_cooldown", [first, during(), during(), after]);
    let rig = fast_session(&world);
    let before = main_role(&rig);
    assert_eq!(
        (before.speed_in_effect, before.speed_cooldown_until),
        (Speed::Fast, None)
    );

    world.run(async {
        rig.prompt("go");
        rig.events.seen("user", 1).await;
        world.cooldowns.start(
            &ProviderId::new(fixtures::PROVIDER),
            &ModelId::new(fixtures::MODEL),
            &Speed::Fast,
            Duration::from_secs(600),
        );
        refused.open();
        rig.events.turns(1).await;
    });

    let labels: Vec<String> = rig
        .events
        .labels()
        .into_iter()
        .filter(|label| label.starts_with("notice:"))
        .collect();
    assert_eq!(
        labels,
        ["notice:speed_fallback", "notice:speed_cooldown:started"]
    );
    let fallback = &fallbacks(&rig)[0];
    assert_eq!(fallback.by, SpeedFallbackBy::Harness);
    assert_eq!(fallback.reason, SpeedFallbackReason::RateLimited);
    assert_eq!(fallback.cooldown_until.as_ref().unwrap().as_str(), UNTIL);
    assert_eq!(
        fallback.message,
        "Fast mode is rate limited. Running at standard speed until 2026-11-13T10:10:00Z."
    );

    // The setting is still fast. What is in effect is standard, until the
    // cooldown's end, and front ends were told.
    for role in [main_role(&rig), told(&rig)] {
        assert_eq!(role.speed, Speed::Fast);
        assert_eq!(role.speed_in_effect, Speed::Standard);
        assert_eq!(role.speed_cooldown_until.as_ref().unwrap().as_str(), UNTIL);
    }

    // Two more requests during the cooldown: the same cooldown is not told
    // again.
    turn(&world, &rig, 2);
    turn(&world, &rig, 3);
    assert_eq!(fallbacks(&rig).len(), 1);
    assert!(
        world
            .provider
            .requests()
            .iter()
            .all(|request| request.speed == Speed::Fast),
        "the loop keeps asking for what is set; the client decides what is sent"
    );

    // The cooldown's time passes. Nothing is timed: the state reads the
    // table when it is asked.
    world.clock.advance(Duration::from_secs(600));
    let passed = main_role(&rig);
    assert_eq!(
        (passed.speed_in_effect, passed.speed_cooldown_until),
        (Speed::Fast, None)
    );

    turn(&world, &rig, 4);
    let notices = rig.events.notices();
    let Some(Notice::SpeedCooldown(ended)) = notices.last() else {
        panic!("{notices:#?}");
    };
    assert_eq!(ended.state, CooldownState::Ended);
    assert_eq!(ended.message, "Fast mode is back.");
    assert_eq!(told(&rig).speed_in_effect, Speed::Fast);
}

#[test]
fn a_tier_the_model_does_not_offer_is_kept_as_the_setting_and_told_once() {
    let not_offered = || {
        Response::events([fell_back(SpeedFallbackReason::NotOffered, None), start()])
            .served_at(Speed::Standard)
            .text("At standard.")
            .end(StopReason::End)
    };
    let world = World::new("speed_not_offered", [not_offered(), not_offered()]);
    let mut setup = Setup::new(&world);
    setup.speeds.session = Speed::Fast;
    setup.model = RoleModel::new(setup.model.client.clone(), fixtures::plain_model());
    let rig = setup.start(&world);

    let role = main_role(&rig);
    assert_eq!(role.speed, Speed::Fast, "the setting is kept");
    assert_eq!(
        (role.speed_in_effect, role.speed_cooldown_until),
        (Speed::Standard, None)
    );

    turn(&world, &rig, 1);
    turn(&world, &rig, 2);
    let notices = fallbacks(&rig);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].reason, SpeedFallbackReason::NotOffered);
    assert!(
        notices[0].message.contains("does not offer fast speed"),
        "{}",
        notices[0].message
    );

    // Another tier it does not offer is not in effect either.
    rig.handle.set_speed(Speed::Other("priority".to_owned()));
    assert_eq!(main_role(&rig).speed_in_effect, Speed::Standard);
}
