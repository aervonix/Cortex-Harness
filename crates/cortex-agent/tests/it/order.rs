//! The rig's seeded order of events (`docs/testing-and-ci.md`, "A seeded
//! order of events"): one seed is one order, another seed can be another,
//! and a test that depends on one order is found by `cargo xtask stress`.

use std::collections::BTreeSet;

use serde_json::json;

use crate::{Setup, World, calls, says};

/// Two sessions on one core, each with a turn that makes one call. The two
/// calls are planned at the same time, and whichever plan arrives first
/// starts first: that is the seed's to decide, and the loop promises
/// neither order. Returns the tools' log and each session's events.
fn two_sessions(world: &World) -> (Vec<String>, Vec<String>, Vec<String>) {
    let first = Setup::new(world).start(world);
    let mut setup = Setup::new(world);
    setup.id = "s_2".to_owned();
    let second = setup.start(world);
    world.run(async {
        first.prompt("go");
        second.prompt("go");
        first.events.turns(1).await;
        second.events.turns(1).await;
    });
    (
        world.log.lines(),
        first.events.every_label(),
        second.events.every_label(),
    )
}

fn one_call_each() -> [cortex_testkit::Response; 4] {
    [
        calls(&[("c1", "read", json!({"path": "a"}))]),
        calls(&[("c2", "read", json!({"path": "b"}))]),
        says("Done."),
        says("Done."),
    ]
}

#[test]
fn one_seed_is_one_order_of_events() {
    for seed in 0..24 {
        let first = two_sessions(&World::seeded("one_seed_first", seed, one_call_each()));
        let second = two_sessions(&World::seeded("one_seed_second", seed, one_call_each()));

        assert_eq!(first, second, "seed {seed}");
    }
}

#[test]
fn another_seed_can_be_another_order() {
    let logs: BTreeSet<Vec<String>> = (0..24)
        .map(|seed| two_sessions(&World::seeded("another_seed", seed, one_call_each())).0)
        .collect();

    assert!(logs.len() > 1, "24 seeds gave one order of calls: {logs:?}");
    // Whatever the order, both calls ran, once each.
    for log in &logs {
        let mut sorted = log.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            ["end read a", "end read b", "start read a", "start read b"]
        );
    }
}

#[test]
fn whatever_the_order_a_rounds_results_are_recorded_in_the_models() {
    for seed in 0..24 {
        let responses = [
            calls(&[
                ("c1", "read", json!({"path": "a"})),
                ("c2", "read", json!({"path": "b"})),
                ("c3", "peek", json!({"path": "c"})),
                ("c4", "plain", json!({"id": "d"})),
            ]),
            says("Done."),
        ];
        let world = World::seeded("recorded_in_order", seed, responses);
        let rig = Setup::new(&world).start(&world);
        world.run(async {
            rig.prompt("go");
            rig.events.turns(1).await;
        });

        let results: Vec<String> = rig
            .events
            .labels()
            .into_iter()
            .filter(|label| label.starts_with("result:"))
            .collect();
        assert_eq!(
            results,
            [
                "result:read:success",
                "result:read:success",
                "result:peek:success",
                "result:plain:success"
            ],
            "seed {seed}"
        );
    }
}

/// A test that is wrong on purpose: it expects the first session's call to
/// start before the second's, which nothing promises. It passes under some
/// seeds and fails under others, and is what `cargo xtask stress` is tried
/// on (`xtask/src/stress.rs`). It is ignored, so no ordinary run and no CI
/// job runs it.
#[test]
#[ignore = "fails on purpose under some seeds; `cargo xtask stress` is tested on it"]
fn stress_fixture_expects_the_first_sessions_call_to_start_first() {
    let world = World::new("stress_fixture", one_call_each());

    let (log, _, _) = two_sessions(&world);

    assert_eq!(
        log.first().map(String::as_str),
        Some("start read a"),
        "seed {}: {log:?}",
        world.seed
    );
}
