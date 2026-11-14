//! [`Rig::fill`]: a session's first request sized to a named pressure from
//! what the session really sends, not from bytes counted by hand. Each
//! pressure is checked against what the loop then does, with the fake
//! tools and with the real ones, and with instruction files that the twin
//! must count too.

use std::sync::Arc;

use cortex_agent::{ContextSettings, RoleModel, StatEvent};
use cortex_protocol::event::TurnOutcome;
use cortex_testkit::{ScriptedProvider, fixtures};
use cortex_tools::{
    EditTool, Editing, ListTool, MemoryCheckpoints, PlanBoard, PlanTool, ReadTool, SearchTool,
    Toolbox, WriteTool,
};

use crate::{Fill, Filled, Rig, Setup, World, says};

/// The real tools that need no shell: what their declarations weigh is
/// what a session of the harness sends.
fn real_tools(world: &World) -> Toolbox {
    let editing = Editing::new(Arc::new(MemoryCheckpoints::new()), world.shared_clock());
    Toolbox::new()
        .with(ReadTool::new())
        .with(ListTool::new())
        .with(SearchTool::new())
        .with(WriteTool::new(editing.clone()))
        .with(EditTool::new(editing))
        .with(PlanTool::new(Arc::new(PlanBoard::new())))
}

/// The estimate of the session's first request, from its context record.
fn first_estimate(rig: &Rig) -> u64 {
    rig.stats
        .all()
        .into_iter()
        .find_map(|event| match event {
            StatEvent::Context(context) => Some(context.estimate),
            _ => None,
        })
        .expect("a request was sent")
}

/// The ways a session is put together that change its first request's
/// size: the fake tools, the real ones, and project instructions.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Fake,
    Real,
    Instructed,
}

const KINDS: [Kind; 3] = [Kind::Fake, Kind::Real, Kind::Instructed];

fn setup(world: &World, kind: Kind, auto_compact: bool, fast: Option<&ScriptedProvider>) -> Rig {
    let mut setup = Setup::new(world);
    setup.context = ContextSettings {
        auto_compact,
        ..ContextSettings::default()
    };
    if let Some(fast) = fast {
        setup.utility = Some(RoleModel::new(Arc::new(fast.clone()), fixtures::model()));
    }
    match kind {
        Kind::Fake => {}
        Kind::Real => setup.toolbox = Some(real_tools(world)),
        Kind::Instructed => {}
    }
    let rig = setup.start(world);
    if matches!(kind, Kind::Instructed) {
        let root =
            cortex_base::AbsPath::from_os(&std::fs::canonicalize(world.dir.join("s_1")).unwrap())
                .unwrap();
        rig.instructions.set(
            &root.join("AGENTS.md").unwrap(),
            "Follow the house rules. ".repeat(40),
        );
    }
    rig
}

#[test]
fn below_the_threshold_the_request_goes_as_it_is() {
    for kind in KINDS {
        let world = World::new("fill_below", [says("Fine.")]);
        let rig = setup(&world, kind, true, None);
        rig.window(6_000);
        let filled = rig.fill(Fill::Below, "go");
        world.run(async {
            rig.prompt("go");
            rig.events.turns(1).await;
        });
        let estimate = first_estimate(&rig);
        assert!(
            estimate.abs_diff(filled.estimate) <= 1,
            "{kind:?}: measured {estimate}, filled {filled:?}"
        );
        assert!(
            (estimate as f64) < filled.prepare,
            "{kind:?}: {estimate} is past the threshold: {filled:?}"
        );
        assert_eq!(world.provider.requests().len(), 1, "{kind:?}");
    }
}

#[test]
fn preparing_starts_a_summary_in_the_background() {
    for kind in KINDS {
        let world = World::new("fill_preparing", [says("Fine.")]);
        let fast = ScriptedProvider::new([says("A summary.")]);
        let rig = setup(&world, kind, true, Some(&fast));
        rig.window(6_000);
        let filled = rig.fill(Fill::Preparing, "go");
        world.run(async {
            rig.prompt("go");
            rig.events.turns(1).await;
        });
        let estimate = first_estimate(&rig) as f64;
        assert!(
            estimate > filled.prepare && estimate < filled.compact,
            "{kind:?}: {estimate} is not between the thresholds: {filled:?}"
        );
        assert_eq!(world.provider.requests().len(), 1, "{kind:?}");
        assert_eq!(
            fast.requests().len(),
            1,
            "{kind:?}: no summary was prepared"
        );
    }
}

/// Past the trigger, and past the window: with automatic compaction off
/// the turn stops before anything is sent, which it does only past the
/// trigger.
#[test]
fn past_the_trigger_and_past_the_window_the_turn_asks_for_a_compaction() {
    for fill in [Fill::Compacting, Fill::PastWindow] {
        for kind in KINDS {
            let world = World::new("fill_past", [says("Unreached.")]);
            let rig = setup(&world, kind, false, None);
            rig.window(6_000);
            let filled: Filled = rig.fill(fill, "go");
            #[expect(clippy::cast_precision_loss, reason = "small token counts")]
            let estimate = filled.estimate as f64;
            match fill {
                Fill::Compacting => assert!(
                    estimate > filled.compact && filled.estimate < filled.limit,
                    "{kind:?}: {filled:?}"
                ),
                _ => assert!(filled.estimate > filled.window, "{kind:?}: {filled:?}"),
            }
            world.run(async {
                rig.prompt("go");
                rig.events.turns(1).await;
            });
            let end = &rig.events.ends()[0];
            assert_eq!(end.outcome, TurnOutcome::Failed, "{kind:?} {fill:?}");
            assert_eq!(
                end.error.as_ref().unwrap().code,
                "compaction_needed",
                "{kind:?} {fill:?}"
            );
            assert!(world.provider.requests().is_empty(), "{kind:?} {fill:?}");
        }
    }
}

#[test]
fn the_twin_counts_the_tools_and_the_instructions() {
    let base = |kind| {
        let world = World::new("fill_base", [says("Fine.")]);
        let rig = setup(&world, kind, true, None);
        rig.fill(Fill::Below, "go").base_bytes
    };
    let (fake, real, instructed) = (base(Kind::Fake), base(Kind::Real), base(Kind::Instructed));
    assert!(
        real > fake + 1_000,
        "the real tools weigh more: {real} {fake}"
    );
    assert!(
        instructed >= fake + 900,
        "the instructions are in the prompt: {instructed} {fake}"
    );
}

#[test]
#[should_panic(expected = "cannot be reached")]
fn a_pressure_the_window_cannot_hold_is_refused() {
    let world = World::new("fill_too_small", [says("Unreached.")]);
    let rig = setup(&world, Kind::Real, true, None);
    // The real tools alone are past the trigger of so small a window.
    rig.window(800);
    let _ = rig.fill(Fill::Below, "go");
}
