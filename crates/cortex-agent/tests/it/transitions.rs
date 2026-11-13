//! Every change of state the loop makes, each shown by a scripted turn
//! (`docs/architecture/03-agent-loop.md`, "Turn state machine").
//!
//! A scenario is a function that runs one small session and returns the
//! states it went through. Each has a test that asserts the whole walk.
//! The last test runs them all and requires that, between them, they make
//! exactly the changes of state in the loop's table: an edge with no
//! scenario fails it, and so does a scenario that finds an edge the table
//! does not have.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use cortex_agent::{Ledger, RepetitionLimits, TRANSITIONS, TurnState, VerifyOutcome};
use cortex_base::{ErrorKind, UtcTime};
use cortex_llm::error::ProviderError;
use cortex_llm::{ModelEvent, RetryInfo, SpeedMove};
use cortex_protocol::lease::Budget;
use cortex_protocol::speed::{SpeedFallbackBy, SpeedFallbackReason};
use cortex_protocol::{Speed, StopReason};
use cortex_testkit::{Gate, Response};
use serde_json::json;

use crate::{Rig, Setup, World, calls, lock, says};

use TurnState::{
    Backoff, Compacting, Deciding, Finishing, Idle, Interrupted, Preparing, Requesting, Streaming,
    ToolRound, Verifying,
};

/// An attempt failed and another follows a wait.
pub(crate) fn retry(attempt: u32) -> ModelEvent {
    ModelEvent::Retry(RetryInfo {
        attempt,
        max_attempts: 5,
        error: ProviderError::new(ErrorKind::Transient, "overloaded", "The provider is busy."),
        wait: Duration::from_secs(2),
        next_attempt_at: UtcTime::parse("2026-11-13T10:00:02Z").unwrap(),
    })
}

/// A response began.
pub(crate) fn start() -> ModelEvent {
    ModelEvent::Start {
        response_id: None,
        model: "test-model".to_owned(),
    }
}

/// The request was moved to standard speed by the harness.
pub(crate) fn fell_back(reason: SpeedFallbackReason, until: Option<&str>) -> ModelEvent {
    ModelEvent::SpeedFallback(SpeedMove {
        requested: Speed::Fast,
        served: Speed::Standard,
        by: SpeedFallbackBy::Harness,
        reason,
        cooldown_until: until.map(|time| UtcTime::parse(time).unwrap()),
    })
}

pub(crate) fn overflow() -> ProviderError {
    ProviderError::new(
        ErrorKind::ContextOverflow,
        "context_length_exceeded",
        "The prompt is too long.",
    )
}

pub(crate) fn refused() -> ProviderError {
    ProviderError::new(
        ErrorKind::Provider,
        "invalid_request",
        "The request was refused.",
    )
}

/// Runs one prompt to the end of its turn and returns the states.
fn walk(world: &World, rig: &Rig) -> Vec<TurnState> {
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    rig.seen.states()
}

fn plain(name: &str) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, [says("Hello.")]);
    walk(&world, &rig)
}

fn tool_round(name: &str) -> Vec<TurnState> {
    let (world, rig) = Rig::one(
        name,
        [
            calls(&[("c1", "read", json!({"path": "a"}))]),
            says("Done."),
        ],
    );
    walk(&world, &rig)
}

fn compaction_for_budget(name: &str) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, [says("Hello.")]);
    *lock(&rig.context.over_budget) = true;
    lock(&rig.context.compactions).push_back(Ok(()));
    walk(&world, &rig)
}

fn compaction_fails(name: &str) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, []);
    *lock(&rig.context.over_budget) = true;
    walk(&world, &rig)
}

fn limit_reached(name: &str) -> Vec<TurnState> {
    let world = World::new(name, []);
    let mut setup = Setup::new(&world);
    setup.ledger = Some(Ledger::new(world.shared_clock()).with_turn_budget(Budget {
        requests: Some(0),
        ..Budget::default()
    }));
    let rig = setup.start(&world);
    walk(&world, &rig)
}

/// Interrupts the turn the moment it enters `state`.
fn interrupted_at(name: &str, state: TurnState, responses: Vec<Response>) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, responses);
    interrupt_when(&world, &rig, state)
}

fn interrupt_when(world: &World, rig: &Rig, state: TurnState) -> Vec<TurnState> {
    rig.seen.interrupt_at(state, &rig.handle);
    walk(world, rig)
}

fn interrupted_compacting(name: &str) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, []);
    *lock(&rig.context.over_budget) = true;
    *lock(&rig.context.gate) = Some(Gate::new());
    interrupt_when(&world, &rig, Compacting)
}

fn retry_before_the_start(name: &str) -> Vec<TurnState> {
    let response = Response::events([retry(2), start()])
        .text("Hello.")
        .end(StopReason::End);
    let (world, rig) = Rig::one(name, [response]);
    walk(&world, &rig)
}

/// The provider says the request does not fit, at the point `first` makes
/// it say so, and the compaction that answers it works.
fn overflow_then_compaction(name: &str, first: Response) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, [first, says("Hello.")]);
    lock(&rig.context.compactions).push_back(Ok(()));
    walk(&world, &rig)
}

fn retry_part_way(name: &str) -> Vec<TurnState> {
    let response = Response::new()
        .text("Half an ans")
        .event(retry(2))
        .event(start())
        .text("The whole answer.")
        .end(StopReason::End);
    let (world, rig) = Rig::one(name, [response]);
    walk(&world, &rig)
}

fn fallback_part_way(name: &str) -> Vec<TurnState> {
    let response = Response::new()
        .text("Half an ans")
        .event(fell_back(
            SpeedFallbackReason::Overloaded,
            Some("2026-11-13T10:10:00Z"),
        ))
        .event(start())
        .text("The whole answer.")
        .end(StopReason::End);
    let (world, rig) = Rig::one(name, [response]);
    walk(&world, &rig)
}

/// A turn that writes a file and then ends, so that verification runs.
fn verified(name: &str, outcomes: Vec<VerifyOutcome>, responses: Vec<Response>) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, responses);
    lock(&rig.verifier.outcomes).extend(outcomes);
    walk(&world, &rig)
}

fn writes() -> Response {
    calls(&[("c1", "write", json!({"path": "a"}))])
}

fn interrupted_verifying(name: &str) -> Vec<TurnState> {
    let (world, rig) = Rig::one(name, [writes(), says("Done.")]);
    *lock(&rig.verifier.gate) = Some(Gate::new());
    interrupt_when(&world, &rig, Verifying)
}

fn repetition_stops(name: &str) -> Vec<TurnState> {
    let again = || calls(&[("c1", "read", json!({"path": "a"}))]);
    let world = World::new(name, [again(), again()]);
    let mut setup = Setup::new(&world);
    setup.config.repetition = Some(RepetitionLimits {
        warn_at: 1,
        stop_at: 2,
    });
    let rig = setup.start(&world);
    walk(&world, &rig)
}

fn queued_prompt_follows(name: &str) -> Vec<TurnState> {
    let gate = Gate::new();
    let first = Response::new()
        .text("One.")
        .wait(&gate)
        .end(StopReason::End);
    let (world, rig) = Rig::one(name, [first, says("Two.")]);
    world.run(async {
        rig.prompt("first");
        rig.events.seen("delta:text", 1).await;
        rig.queue("second");
        gate.open();
        rig.events.turns(2).await;
    });
    rig.seen.states()
}

/// A scenario and the name its directory gets.
type Scenario = (&'static str, fn(&str) -> Vec<TurnState>);

/// Every scenario. The coverage test runs each of them.
const SCENARIOS: &[Scenario] = &[
    ("plain", plain),
    ("tool_round", tool_round),
    ("compaction_for_budget", compaction_for_budget),
    ("compaction_fails", compaction_fails),
    ("limit_reached", limit_reached),
    ("interrupted_preparing", |name| {
        interrupted_at(name, Preparing, vec![])
    }),
    ("interrupted_compacting", interrupted_compacting),
    ("retry_before_the_start", retry_before_the_start),
    ("overflow_before_the_start", |name| {
        overflow_then_compaction(name, Response::events([ModelEvent::Error(overflow())]))
    }),
    ("failure_before_the_start", |name| {
        let (world, rig) = Rig::one(name, [Response::events([ModelEvent::Error(refused())])]);
        walk(&world, &rig)
    }),
    ("interrupted_requesting", |name| {
        interrupted_at(name, Requesting, vec![Response::events([]).stall()])
    }),
    ("retry_part_way", retry_part_way),
    ("fallback_part_way", fallback_part_way),
    ("overflow_part_way", |name| {
        overflow_then_compaction(name, Response::new().text("Half").error(overflow()))
    }),
    ("failure_part_way", |name| {
        let (world, rig) = Rig::one(name, [Response::new().text("Half").error(refused())]);
        walk(&world, &rig)
    }),
    ("interrupted_streaming", |name| {
        interrupted_at(name, Streaming, vec![Response::new().text("Half").stall()])
    }),
    ("interrupted_backoff", |name| {
        interrupted_at(name, Backoff, vec![Response::events([retry(2)]).stall()])
    }),
    ("verification_passes", |name| {
        verified(
            name,
            vec![VerifyOutcome::Passed],
            vec![writes(), says("Done.")],
        )
    }),
    ("verification_feeds_back", |name| {
        let failed = VerifyOutcome::Failed {
            report: "test_a failed".to_owned(),
        };
        verified(
            name,
            vec![failed, VerifyOutcome::Passed],
            vec![writes(), says("Done."), says("Fixed.")],
        )
    }),
    ("interrupted_verifying", interrupted_verifying),
    ("paused_response_continues", |name| {
        let paused = Response::new().text("Searching").end(StopReason::Paused);
        let (world, rig) = Rig::one(name, [paused, says("Found it.")]);
        walk(&world, &rig)
    }),
    ("overflow_as_a_stop_reason", |name| {
        overflow_then_compaction(name, Response::new().end(StopReason::ContextOverflow))
    }),
    ("repetition_stops", repetition_stops),
    ("interrupted_tool_round", |name| {
        let waits = calls(&[("c1", "read", json!({"path": "a", "gate": "never"}))]);
        interrupted_at(name, ToolRound, vec![waits])
    }),
    ("queued_prompt_follows", queued_prompt_follows),
];

fn run(scenario: &str, test: &str) -> Vec<TurnState> {
    let (_, function) = SCENARIOS
        .iter()
        .find(|(name, _)| *name == scenario)
        .unwrap_or_else(|| panic!("no scenario `{scenario}`"));
    function(&format!("{test}-{scenario}"))
}

#[test]
fn a_turn_with_no_tools_goes_straight_through() {
    assert_eq!(
        run("plain", "walk"),
        [
            Idle, Preparing, Requesting, Streaming, Deciding, Finishing, Idle
        ]
    );
}

#[test]
fn tool_calls_make_a_round_and_then_another_request() {
    assert_eq!(
        run("tool_round", "walk"),
        [
            Idle, Preparing, Requesting, Streaming, Deciding, ToolRound, Preparing, Requesting,
            Streaming, Deciding, Finishing, Idle
        ]
    );
}

#[test]
fn a_context_over_budget_is_compacted_before_the_request() {
    assert_eq!(
        run("compaction_for_budget", "walk"),
        [
            Idle, Preparing, Compacting, Preparing, Requesting, Streaming, Deciding, Finishing,
            Idle
        ]
    );
    assert_eq!(
        run("compaction_fails", "walk"),
        [Idle, Preparing, Compacting, Finishing, Idle]
    );
}

#[test]
fn a_limit_ends_the_turn_before_anything_is_sent() {
    assert_eq!(
        run("limit_reached", "walk"),
        [Idle, Preparing, Finishing, Idle]
    );
}

#[test]
fn a_failed_attempt_is_a_wait_and_then_the_request_again() {
    assert_eq!(
        run("retry_before_the_start", "walk"),
        [
            Idle, Preparing, Requesting, Backoff, Requesting, Streaming, Deciding, Finishing, Idle
        ]
    );
    assert_eq!(
        run("retry_part_way", "walk"),
        [
            Idle, Preparing, Requesting, Streaming, Backoff, Requesting, Streaming, Deciding,
            Finishing, Idle
        ]
    );
}

#[test]
fn a_fallback_part_way_sends_the_request_again_without_a_wait() {
    assert_eq!(
        run("fallback_part_way", "walk"),
        [
            Idle, Preparing, Requesting, Streaming, Requesting, Streaming, Deciding, Finishing,
            Idle
        ]
    );
}

#[test]
fn a_request_that_does_not_fit_is_compacted_once_wherever_it_is_noticed() {
    assert_eq!(
        run("overflow_before_the_start", "walk"),
        [
            Idle, Preparing, Requesting, Compacting, Preparing, Requesting, Streaming, Deciding,
            Finishing, Idle
        ]
    );
    assert_eq!(
        run("overflow_part_way", "walk"),
        [
            Idle, Preparing, Requesting, Streaming, Compacting, Preparing, Requesting, Streaming,
            Deciding, Finishing, Idle
        ]
    );
    assert_eq!(
        run("overflow_as_a_stop_reason", "walk"),
        [
            Idle, Preparing, Requesting, Streaming, Deciding, Compacting, Preparing, Requesting,
            Streaming, Deciding, Finishing, Idle
        ]
    );
}

#[test]
fn a_request_that_fails_for_good_ends_the_turn() {
    assert_eq!(
        run("failure_before_the_start", "walk"),
        [Idle, Preparing, Requesting, Finishing, Idle]
    );
    assert_eq!(
        run("failure_part_way", "walk"),
        [Idle, Preparing, Requesting, Streaming, Finishing, Idle]
    );
}

#[test]
fn verification_runs_when_the_turn_wrote_files() {
    let written_then_done = [
        Idle, Preparing, Requesting, Streaming, Deciding, ToolRound, Preparing, Requesting,
        Streaming, Deciding, Verifying,
    ];
    let mut passes = written_then_done.to_vec();
    passes.extend([Finishing, Idle]);
    assert_eq!(run("verification_passes", "walk"), passes);

    // A failure goes back to the model, and its answer is verified again.
    let mut feeds_back = written_then_done.to_vec();
    feeds_back.extend([
        Preparing, Requesting, Streaming, Deciding, Verifying, Finishing, Idle,
    ]);
    assert_eq!(run("verification_feeds_back", "walk"), feeds_back);
}

#[test]
fn a_paused_response_is_continued_by_the_next_request() {
    assert_eq!(
        run("paused_response_continues", "walk"),
        [
            Idle, Preparing, Requesting, Streaming, Deciding, Preparing, Requesting, Streaming,
            Deciding, Finishing, Idle
        ]
    );
}

#[test]
fn a_round_that_keeps_repeating_ends_the_turn() {
    let round = [Preparing, Requesting, Streaming, Deciding, ToolRound];
    let mut expected = vec![Idle];
    expected.extend(round);
    expected.extend(round);
    expected.extend([Finishing, Idle]);
    assert_eq!(run("repetition_stops", "walk"), expected);
}

#[test]
fn a_queued_prompt_starts_the_next_turn_without_going_idle() {
    let turn = [Preparing, Requesting, Streaming, Deciding, Finishing];
    let mut expected = vec![Idle];
    expected.extend(turn);
    expected.extend(turn);
    expected.push(Idle);
    assert_eq!(run("queued_prompt_follows", "walk"), expected);
}

#[test]
fn an_interruption_is_taken_in_every_state_that_waits() {
    let ends = |scenario: &str, before: &[TurnState]| {
        let mut expected = vec![Idle];
        expected.extend(before);
        expected.extend([Interrupted, Idle]);
        assert_eq!(run(scenario, "walk"), expected, "{scenario}");
    };
    ends("interrupted_preparing", &[Preparing]);
    ends("interrupted_compacting", &[Preparing, Compacting]);
    ends("interrupted_requesting", &[Preparing, Requesting]);
    ends("interrupted_streaming", &[Preparing, Requesting, Streaming]);
    ends("interrupted_backoff", &[Preparing, Requesting, Backoff]);
    ends(
        "interrupted_tool_round",
        &[Preparing, Requesting, Streaming, Deciding, ToolRound],
    );
    ends(
        "interrupted_verifying",
        &[
            Preparing, Requesting, Streaming, Deciding, ToolRound, Preparing, Requesting,
            Streaming, Deciding, Verifying,
        ],
    );
}

#[test]
fn the_scenarios_make_every_transition_in_the_table_and_no_other() {
    let mut made: BTreeSet<(TurnState, TurnState)> = BTreeSet::new();
    for (scenario, _) in SCENARIOS {
        let states = run(scenario, "cover");
        made.extend(states.windows(2).map(|pair| (pair[0], pair[1])));
    }
    let table: BTreeSet<(TurnState, TurnState)> = TRANSITIONS.iter().copied().collect();
    let untested: Vec<_> = table.difference(&made).collect();
    let unlisted: Vec<_> = made.difference(&table).collect();
    assert!(
        untested.is_empty(),
        "transitions with no scenario: {untested:?}"
    );
    assert!(
        unlisted.is_empty(),
        "transitions the table does not have: {unlisted:?}"
    );
}

#[test]
fn an_observer_sees_the_session_and_the_handle_agrees_with_it() {
    let (world, rig) = Rig::one("observer", [says("Hello.")]);
    assert_eq!(rig.handle.turn_state(), Idle);
    walk(&world, &rig);
    assert_eq!(rig.handle.turn_state(), Idle);
    assert_eq!(rig.handle.id().as_str(), "s_1");
    let _ = Arc::clone(&rig.seen);
}
