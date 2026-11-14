//! Several sessions on one core, a session that is resumed, a store that
//! fails, and the stages behind traits: statistics, verification and
//! compaction.

use std::sync::{Arc, Mutex};

use cortex_agent::{
    ContextStat, MemorySessionStore, NOTICE_LEAD, Opened, PrefixDifference, Record, SessionStore,
    StatEvent, StoreError, ToolStatOutcome, VerifyOutcome,
};
use cortex_llm::ModelEvent;
use cortex_protocol::error::code;
use cortex_protocol::event::{Notice, TurnCompleted, TurnOutcome, Verification};
use cortex_protocol::item::{AssistantMessageItem, NoticeSource, UserMessageItem};
use cortex_protocol::lease::Level;
use cortex_protocol::model::ModelRole;
use cortex_protocol::usage::{LimitMeasure, LimitWindow};
use cortex_protocol::{ErrorKind, Item, ItemId, Part, StopReason, TurnId};
use cortex_testkit::{Response, ScriptedProvider};
use cortex_tools::BoxFuture;
use serde_json::json;

use crate::transitions::overflow;
use crate::{
    DATE, Rig, Setup, World, assert_valid_history, calls, last_request, lock, says, text_of,
};

fn turn(world: &World, rig: &Rig, turns: usize) {
    world.run(async {
        rig.prompt("go");
        rig.events.turns(turns).await;
    });
}

// ------------------------------------------------------- several sessions

#[test]
fn sessions_on_one_core_run_side_by_side_and_do_not_touch_each_other() {
    let world = World::new("two_sessions", []);
    let session = |id: &str, responses: Vec<Response>| {
        let mut setup = Setup::new(&world);
        setup.id = id.to_owned();
        setup.model.client = Arc::new(ScriptedProvider::new(responses));
        setup.start(&world)
    };
    let first = session(
        "s_a",
        vec![calls(&[(
            "c1",
            "read",
            json!({"path": "a", "gate": "held"}),
        )])],
    );
    let second = session("s_b", vec![says("One."), says("Two.")]);

    world.run(async {
        first.prompt("in a");
        world.log.has("start read a").await;
        // The first session is in the middle of a tool round. The second
        // runs a whole turn meanwhile, on the same thread.
        second.prompt("in b");
        second.events.seen("idle", 1).await;
        assert_eq!(first.events.ends().len(), 0);

        // Interrupting one leaves the other alone.
        let _ = first.handle.interrupt();
        first.events.turns(1).await;
        second.prompt("in b again");
        second.events.seen("idle", 2).await;
    });

    assert_eq!(first.events.ends()[0].outcome, TurnOutcome::Interrupted);
    let outcomes: Vec<TurnOutcome> = second
        .events
        .ends()
        .into_iter()
        .map(|end| end.outcome)
        .collect();
    assert_eq!(outcomes, vec![TurnOutcome::Completed; 2]);
    // Each has its own events, numbered from one, and its own ids.
    for rig in [&first, &second] {
        let events = rig.events.all();
        assert!(
            events
                .iter()
                .all(|event| event.session_id == *rig.handle.id())
        );
        assert!(
            events
                .iter()
                .enumerate()
                .all(|(index, event)| event.seq == index as u64 + 1)
        );
        assert_eq!(events[1].turn_id.as_ref().unwrap().as_str(), "t_1");
    }
    assert_eq!(
        first.store.items().len(),
        3,
        "its own log: prompt, message, result"
    );
    assert_eq!(second.store.items().len(), 4);
}

#[test]
fn a_sessions_state_says_what_is_running() {
    let (world, rig) = Rig::one("session_state", [Response::new().text("x").stall()]);
    let idle = rig.handle.state();
    assert_eq!(idle.session_id.as_str(), "s_1");
    assert_eq!(idle.workspace, "/work/test");
    assert_eq!(idle.level, Level::Supervised);
    assert_eq!(idle.created_at.as_str(), "2026-11-13T10:00:00Z");
    assert_eq!(idle.roles[&ModelRole::Main].model.as_str(), "test-model");
    assert_eq!(
        (idle.running_turn.clone(), idle.queue_length, idle.seq),
        (None, 0, 0)
    );

    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        let running = rig.handle.state();
        assert_eq!(running.running_turn.unwrap().as_str(), "t_1");
        assert_eq!(running.seq, rig.events.all().len() as u64);
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
}

// ---------------------------------------------------------------- resume

#[test]
fn a_resumed_session_sends_its_history_and_continues_its_ids() {
    let earlier = [
        Item::UserMessage(UserMessageItem {
            id: ItemId::new("i_40"),
            parts: vec![Part::text("What is in a?")],
            delivery: None,
            created_at: None,
        }),
        Item::AssistantMessage(AssistantMessageItem {
            id: ItemId::new("i_41"),
            parts: vec![Part::text("Nothing yet.")],
            stop_reason: Some(StopReason::End),
            raw_stop: None,
            stop_detail: None,
            truncated: false,
            origin: None,
            usage: None,
            error: None,
            created_at: None,
        }),
    ];
    let world = World::new("resume", [says("Still nothing.")]);
    let mut setup = Setup::new(&world);
    setup.memory = MemorySessionStore::with_items(&TurnId::new("t_12"), earlier);
    let rig = setup.start(&world);
    world.run(async {
        let started = rig.prompt("And now?");
        assert_eq!(started.turn_id.as_str(), "t_13");
        rig.events.turns(1).await;
    });

    let request = last_request(&world.provider);
    let said: Vec<String> = request.messages.iter().map(text_of).collect();
    assert_eq!(said, ["What is in a?", "Nothing yet.", "And now?"]);
    let ids: Vec<String> = rig
        .events
        .items()
        .iter()
        .map(|item| item.id().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["i_42", "i_43"]);
}

// ------------------------------------------------------- a store that fails

/// The memory store, with appends that can be made to fail.
#[derive(Clone, Default)]
pub struct Flaky {
    pub inner: MemorySessionStore,
    pub appends: Arc<Mutex<usize>>,
    /// The append, counted from 0, that fails.
    pub fail_append: Arc<Mutex<Option<usize>>>,
    fail_end: Arc<Mutex<bool>>,
    /// The epochs of this reason fail to be appended.
    pub fail_epoch: Arc<Mutex<Option<cortex_agent::EpochReason>>>,
    /// Whether opening the session fails. The first open, when the session
    /// starts, is before a test can set it.
    fail_open: Arc<Mutex<bool>>,
    opens: Arc<Mutex<usize>>,
}

impl SessionStore for Flaky {
    fn open(&mut self) -> BoxFuture<'_, Result<Opened, StoreError>> {
        *lock(&self.opens) += 1;
        if *lock(&self.fail_open) {
            return Box::pin(std::future::ready(Err(StoreError::new("the disk is gone"))));
        }
        self.inner.open()
    }

    fn append<'a>(
        &'a mut self,
        turn: &'a TurnId,
        record: Record,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let index = {
            let mut appends = lock(&self.appends);
            *appends += 1;
            *appends - 1
        };
        if *lock(&self.fail_append) == Some(index) {
            return Box::pin(std::future::ready(Err(StoreError::new("the disk is full"))));
        }
        if let Record::Epoch(epoch) = &record
            && epoch.reason.is_some()
            && *lock(&self.fail_epoch) == epoch.reason
        {
            return Box::pin(std::future::ready(Err(StoreError::new("the disk is full"))));
        }
        self.inner.append(turn, record)
    }

    fn end_turn<'a>(
        &'a mut self,
        turn: &'a TurnId,
        outcome: &'a TurnCompleted,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        if *lock(&self.fail_end) {
            return Box::pin(std::future::ready(Err(StoreError::new("the disk is full"))));
        }
        self.inner.end_turn(turn, outcome)
    }
}

fn flaky(world: &World) -> (Flaky, Rig) {
    let store = Flaky::default();
    let mut setup = Setup::new(world);
    setup.memory = store.inner.clone();
    setup.store = Some(Box::new(store.clone()));
    (store, setup.start(world))
}

#[test]
fn a_compaction_whose_epoch_was_not_recorded_goes_on_from_what_the_log_holds() {
    let world = World::new(
        "compaction_epoch_lost",
        [says("First."), says("A summary."), says("Second.")],
    );
    let (store, rig) = flaky(&world);
    rig.window(4_000);
    world.run(async {
        rig.prompt("one");
        rig.events.turns(1).await;
    });
    // The summary is recorded, and the epoch after it is not.
    *lock(&store.fail_epoch) = Some(cortex_agent::EpochReason::Compaction);
    world.run(async {
        rig.bulk(9_000);
        rig.prompt("two");
        rig.events.turns(2).await;
    });
    *lock(&store.fail_epoch) = None;
    world.run(async {
        rig.prompt("three");
        rig.events.turns(3).await;
    });
    let ends = rig.events.ends();
    assert_eq!(ends[1].error.as_ref().unwrap().code, "store_failed");
    assert_eq!(
        ends[2].outcome,
        TurnOutcome::Completed,
        "{:?}",
        ends[2].error
    );
    let first = all_text_of(&world.provider.requests()[2].messages[0]);
    assert!(first.contains("A summary."), "{first}");
}

fn all_text_of(message: &cortex_protocol::Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            cortex_protocol::Part::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_append_that_fails_fails_the_turn_and_the_item_is_not_in_the_context() {
    let world = World::new("store_fails", [says("Lost."), says("Back.")]);
    let (store, rig) = flaky(&world);
    // The epoch and the prompt are appended; the assistant message is not.
    *lock(&store.fail_append) = Some(2);
    turn(&world, &rig, 1);

    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    let error = end.error.as_ref().unwrap();
    assert_eq!(
        (error.kind.clone(), error.code.as_str()),
        (ErrorKind::Internal, "store_failed")
    );
    assert!(
        error.message.contains("the disk is full"),
        "{}",
        error.message
    );
    assert_eq!(rig.store.items().len(), 1, "the log has the prompt only");
    assert!(
        !rig.events.labels().contains(&"assistant".to_owned()),
        "what is not in the log was not shown as complete"
    );

    // The session is read again from its store, so memory holds what the
    // log holds.
    world.run(async {
        rig.prompt("again");
        rig.events.turns(2).await;
    });
    assert_eq!(*lock(&store.opens), 2);
    let request = last_request(&world.provider);
    let said: Vec<String> = request.messages.iter().map(text_of).collect();
    assert_eq!(
        said,
        [format!("{DATE}go"), "again".to_owned()],
        "the lost message is not sent"
    );
    assert_eq!(rig.events.ends()[1].outcome, TurnOutcome::Completed);
}

#[test]
fn a_tool_result_that_cannot_be_appended_fails_the_turn_and_the_session_is_read_again() {
    let world = World::new(
        "store_fails_result",
        [
            calls(&[
                ("c1", "read", json!({"path": "a"})),
                ("c2", "read", json!({"path": "b"})),
            ]),
            says("After."),
        ],
    );
    let (store, rig) = flaky(&world);
    // The epoch, the prompt, the message and the first result are
    // appended. The second result is not: the round is half recorded.
    *lock(&store.fail_append) = Some(4);
    turn(&world, &rig, 1);
    assert_eq!(rig.store.items().len(), 3);
    assert_eq!(
        rig.events.ends()[0].error.as_ref().unwrap().code,
        "store_failed"
    );
    assert_eq!(
        world.provider.requests().len(),
        1,
        "no request goes out on a history with a hole"
    );
    // The real store repairs a call with no result when it is opened. The
    // loop's part is to open it again before anything else is sent.
    assert_eq!(*lock(&store.opens), 1);
    world.run(async {
        rig.prompt("again");
        rig.events.turns(2).await;
    });
    assert_eq!(
        *lock(&store.opens),
        2,
        "once when the session started, once before the turn after the failure"
    );
}

#[test]
fn a_session_that_cannot_be_read_again_fails_its_turns_until_it_can() {
    let world = World::new("reload_fails", [says("Lost."), says("Back.")]);
    let (store, rig) = flaky(&world);
    *lock(&store.fail_append) = Some(2);
    turn(&world, &rig, 1);
    assert_eq!(
        rig.events.ends()[0].error.as_ref().unwrap().code,
        "store_failed"
    );

    // The store cannot be opened either. No request goes out on a history
    // that may have a hole: the turn fails, and nothing is forgotten.
    *lock(&store.fail_open) = true;
    world.run(async {
        rig.prompt("again");
        rig.events.turns(2).await;
    });
    let end = &rig.events.ends()[1];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    assert!(
        end.error
            .as_ref()
            .unwrap()
            .message
            .contains("the disk is gone"),
        "{end:?}"
    );
    assert_eq!(world.provider.requests().len(), 1, "nothing was sent");

    // When the store is back, the next turn reads the session and goes on.
    *lock(&store.fail_open) = false;
    world.run(async {
        rig.prompt("and again");
        rig.events.turns(3).await;
    });
    assert_eq!(rig.events.ends()[2].outcome, TurnOutcome::Completed);
    assert_eq!(*lock(&store.opens), 3);
    let said: Vec<String> = last_request(&world.provider)
        .messages
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(
        said,
        [format!("{DATE}go"), "and again".to_owned()],
        "what the log holds, and the new prompt"
    );
}

/// A verifier with a bug in it.
struct Faulty;

impl cortex_agent::Verifier for Faulty {
    fn verify<'a>(&'a self, _cx: cortex_agent::VerifyCx<'a>) -> BoxFuture<'a, VerifyOutcome> {
        Box::pin(async { panic!("the verifier has a bug") })
    }
}

#[test]
fn a_panic_in_the_loop_ends_the_turn_and_the_session_carries_on() {
    // The loop's own parts are behind traits, and any of them can have a
    // bug. Here the verifier panics.
    let world = World::new(
        "loop_panics",
        [
            calls(&[("c1", "write", json!({"path": "a"}))]),
            says("Done."),
            says("Still here."),
        ],
    );
    let (store, mut setup) = (Flaky::default(), Setup::new(&world));
    setup.memory = store.inner.clone();
    setup.store = Some(Box::new(store.clone()));
    setup.verifier = Some(Arc::new(Faulty));
    let rig = setup.start(&world);
    rig.events
        .loop_panic_expected
        .store(true, std::sync::atomic::Ordering::SeqCst);
    rig.queue_after_start(&world, "queued behind the panic");

    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    let error = end.error.as_ref().unwrap();
    assert_eq!(
        (error.kind.clone(), error.code.as_str()),
        (ErrorKind::Internal, "loop_panicked")
    );
    assert!(
        error.message.contains("the verifier has a bug"),
        "{}",
        error.message
    );
    // Nothing follows by itself, and the session is not stuck: its state
    // says idle, with the queued prompt held.
    let state = rig.handle.state();
    assert_eq!((state.running_turn.clone(), state.queue_length), (None, 1));
    assert_eq!(rig.handle.turn_state(), cortex_agent::TurnState::Idle);

    // The next prompt works, on a session read again from its store, and
    // the held prompt follows it.
    world.provider.push(says("And the queued one."));
    world.run(async {
        rig.prompt("are you there?");
        rig.events.seen("idle", 1).await;
    });
    assert_eq!(*lock(&store.opens), 2);
    let outcomes: Vec<TurnOutcome> = rig
        .events
        .ends()
        .into_iter()
        .map(|end| end.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [
            TurnOutcome::Failed,
            TurnOutcome::Completed,
            TurnOutcome::Completed
        ]
    );
    assert_valid_history(&last_request(&world.provider).messages);
}

#[test]
fn a_turn_whose_end_cannot_be_recorded_is_a_failed_turn() {
    let world = World::new("store_fails_end", [says("Done.")]);
    let (store, rig) = flaky(&world);
    *lock(&store.fail_end) = true;
    turn(&world, &rig, 1);
    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    assert_eq!(end.error.as_ref().unwrap().code, "store_failed");
    assert_eq!(end.verification, None);
}

#[test]
fn a_session_can_be_told_to_read_its_history_again_but_not_under_a_turn() {
    let world = World::new(
        "reload",
        [Response::new().text("x").stall(), says("After.")],
    );
    let (store, rig) = flaky(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.seen("delta:text", 1).await;
        let refused = rig.handle.change_history().unwrap_err();
        assert_eq!(refused.code, code::TURN_RUNNING);
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
    });
    assert_eq!(*lock(&store.opens), 1);
    let change = rig.handle.change_history().expect("the session is idle");
    // While the change is under way no turn starts, and no other change
    // begins: both are decided under the session's one lock.
    let refused = rig
        .handle
        .prompt(vec![Part::text("too soon")], None)
        .unwrap_err();
    assert_eq!(refused.code, code::HISTORY_CHANGING);
    let again = rig.handle.change_history().unwrap_err();
    assert_eq!(again.code, code::HISTORY_CHANGING);
    drop(change);
    world.run(async {
        rig.prompt("after a rewind");
        rig.events.turns(2).await;
    });
    assert_eq!(*lock(&store.opens), 2, "read again before the turn");
    assert_valid_history(&last_request(&world.provider).messages);
    // A closed session's history is not changed.
    rig.handle.close();
    let closed = rig.handle.change_history().unwrap_err();
    assert_eq!(closed.code, code::SESSION_NOT_OPEN);
}

// ------------------------------------------------------------- statistics

#[test]
fn each_request_tool_call_and_turn_ends_with_a_statistics_event() {
    let (world, rig) = Rig::one(
        "stats",
        [
            calls(&[
                ("c1", "read", json!({"path": "a"})),
                ("c2", "read", json!({"path": "b", "fail": true})),
            ]),
            says("Done."),
        ],
    );
    turn(&world, &rig, 1);

    let events = rig.stats.all();
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| match event {
            StatEvent::Request(_) => "request",
            StatEvent::ToolCall(_) => "tool",
            StatEvent::Turn(_) => "turn",
            StatEvent::Context(_) => "context",
            _ => "?",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "request", "context", "tool", "tool", "request", "context", "turn"
        ]
    );
    let StatEvent::Request(first) = &events[0] else {
        panic!("a request");
    };
    assert_eq!(first.stop, StopReason::ToolCalls);
    assert_eq!(first.model.as_str(), "test-model");
    assert_eq!(first.variants.edit_format, "replace");
    assert!(first.usage.is_some() && first.error.is_none());
    assert_eq!(first.at.to_string(), "2026-11-13T10:00:00Z");
    let outcomes: Vec<(&str, ToolStatOutcome)> = events
        .iter()
        .filter_map(|event| match event {
            StatEvent::ToolCall(call) => Some((call.tool.as_str(), call.outcome)),
            _ => None,
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("read", ToolStatOutcome::Ok),
            ("read", ToolStatOutcome::Failed)
        ]
    );
    let StatEvent::Turn(turn) = &events[6] else {
        panic!("a turn");
    };
    assert_eq!((turn.requests, turn.tool_calls), (2, 2));

    // Each request's context record: its estimate by source, its limit,
    // and where it first differs from the request before it.
    let contexts: Vec<&ContextStat> = events
        .iter()
        .filter_map(|event| match event {
            StatEvent::Context(context) => Some(context),
            _ => None,
        })
        .collect();
    let (first, second) = (contexts[0], contexts[1]);
    assert_eq!(first.difference, PrefixDifference::First);
    assert_eq!(second.difference, PrefixDifference::Extends);
    assert!(first.sources.system > 0 && first.sources.tools > 0 && first.sources.user > 0);
    assert_eq!(first.sources.tool_results, 0);
    assert!(second.sources.tool_results > 0 && second.sources.assistant > 0);
    assert!(second.estimate > first.estimate);
    assert_eq!((first.window, first.epoch), (200_000, 1));
    assert!(first.limit < first.window);
    assert!(
        first.usage.is_some(),
        "what the provider counted, when it ended"
    );
    assert_eq!(turn.outcome, TurnOutcome::Completed);
    assert_eq!(turn.verification, Some(Verification::Skipped));
}

#[test]
fn the_limits_a_provider_reports_are_in_the_sessions_usage() {
    let window = LimitWindow {
        measure: LimitMeasure::Requests,
        speed: None,
        limit: Some(1_000),
        remaining: Some(988),
        resets_at: None,
    };
    let response = Response::new()
        .event(ModelEvent::Limits(vec![window.clone()]))
        .text("Done.")
        .end(StopReason::End);
    let (world, rig) = Rig::one("limits_reported", [response]);
    turn(&world, &rig, 1);
    let limits = rig.handle.usage().limits;
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].provider.as_str(), "test");
    assert_eq!(limits[0].windows, [window]);
    assert_eq!(limits[0].seen_at.as_str(), "2026-11-13T10:00:00Z");
}

// ----------------------------------------------------------- verification

fn writes(path: &str) -> Response {
    calls(&[("c1", "write", json!({"path": path}))])
}

#[test]
fn a_failed_check_is_told_to_the_model_which_gets_to_fix_it() {
    let (world, rig) = Rig::one(
        "verify_feedback",
        [writes("a"), says("Done."), writes("b"), says("Fixed.")],
    );
    lock(&rig.verifier.outcomes).extend([
        VerifyOutcome::Failed {
            report: "test_parse failed: expected 2, got 3".to_owned(),
        },
        VerifyOutcome::Passed,
    ]);
    turn(&world, &rig, 1);

    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Completed);
    assert_eq!(end.verification, Some(Verification::Passed));
    // Two rounds, the second with both files written.
    assert_eq!(*lock(&rig.verifier.rounds), [(1, 1), (2, 2)]);
    // The failure is in the conversation, right before the request that
    // answers it.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        text_of(requests[2].messages.last().unwrap()),
        format!("{NOTICE_LEAD}test_parse failed: expected 2, got 3")
    );
    let notes: Vec<_> = rig
        .store
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::Notice(notice) => Some(notice.source),
            _ => None,
        })
        .collect();
    assert_eq!(notes, [NoticeSource::Verification]);
    assert_valid_history(&requests[3].messages);
}

#[test]
fn checks_that_still_fail_at_the_round_limit_end_the_turn_and_say_so() {
    let (world, rig) = Rig::one(
        "verify_round_limit",
        [
            writes("a"),
            says("Done."),
            says("I tried."),
            says("Never sent."),
        ],
    );
    let failed = || VerifyOutcome::Failed {
        report: "still failing".to_owned(),
    };
    lock(&rig.verifier.outcomes).extend([failed(), failed(), failed()]);
    turn(&world, &rig, 1);

    let end = &rig.events.ends()[0];
    assert_eq!(
        end.outcome,
        TurnOutcome::Completed,
        "the turn is over; its checks failed"
    );
    assert_eq!(end.verification, Some(Verification::Failed));
    assert_eq!(
        lock(&rig.verifier.rounds).len(),
        2,
        "two rounds is the limit"
    );
    assert_eq!(
        world.provider.requests().len(),
        3,
        "the second failure is not fed back"
    );
}

#[test]
fn a_turn_that_wrote_nothing_is_not_verified() {
    let (world, rig) = Rig::one(
        "verify_nothing_written",
        [
            calls(&[
                ("c1", "read", json!({"path": "a"})),
                // A write that failed changed nothing.
                ("c2", "write", json!({"path": "b", "fail": true})),
            ]),
            says("Done."),
        ],
    );
    turn(&world, &rig, 1);
    assert!(lock(&rig.verifier.rounds).is_empty());
    assert_eq!(
        rig.events.ends()[0].verification,
        Some(Verification::Skipped)
    );
}

// -------------------------------------------------------------- compaction

fn compaction_notices(rig: &Rig) -> Vec<String> {
    rig.events
        .labels()
        .into_iter()
        .filter(|label| label.starts_with("notice:compaction"))
        .collect()
}

#[test]
fn a_compaction_is_shown_as_it_starts_and_ends() {
    let (world, rig) = Rig::one("compaction_notices", [says("A summary."), says("Done.")]);
    rig.window(4_000);
    rig.bulk(6_000);
    turn(&world, &rig, 1);
    assert_eq!(
        compaction_notices(&rig),
        ["notice:compaction:started", "notice:compaction:finished"]
    );
    assert_eq!(
        rig.store.summaries().len(),
        1,
        "once, though the turn prepared twice"
    );
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    let notices = rig.events.notices();
    let Some(Notice::Compaction(finished)) = notices
        .iter()
        .rev()
        .find(|notice| matches!(notice, Notice::Compaction(_)))
    else {
        panic!("{notices:#?}");
    };
    let (before, after) = (
        finished.before_tokens.unwrap(),
        finished.after_tokens.unwrap(),
    );
    assert!(before > after, "{before} tokens became {after}");
}

#[test]
fn a_request_that_still_does_not_fit_after_one_compaction_ends_the_turn() {
    let too_long = || Response::events([ModelEvent::Error(overflow())]);
    let (world, rig) = Rig::one(
        "overflow_twice",
        [
            too_long(),
            says("A summary."),
            too_long(),
            says("Unreached."),
        ],
    );
    turn(&world, &rig, 1);

    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    assert_eq!(end.error.as_ref().unwrap().kind, ErrorKind::ContextOverflow);
    assert_eq!(
        rig.store.summaries().len(),
        1,
        "compacted once, not again and again"
    );
    assert_eq!(
        world.provider.requests().len(),
        3,
        "the request, the summary, the request"
    );

    // The same when the provider says it as a stop reason.
    let stops = || Response::new().end(StopReason::ContextOverflow);
    let (world, rig) = Rig::one(
        "overflow_twice_stop",
        [stops(), says("A summary."), stops()],
    );
    turn(&world, &rig, 1);
    let end = &rig.events.ends()[0];
    assert_eq!(end.stop_reason, Some(StopReason::ContextOverflow));
    assert_eq!(end.error.as_ref().unwrap().code, "context_overflow");
}

#[test]
fn a_compaction_that_fails_ends_a_turn_whose_request_does_not_fit() {
    // The provider says the request does not fit, and the summary request
    // finds no answer.
    let (world, rig) = Rig::one(
        "overflow_no_compaction",
        [Response::events([ModelEvent::Error(overflow())])],
    );
    turn(&world, &rig, 1);
    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    assert_eq!(end.error.as_ref().unwrap().kind, ErrorKind::ContextOverflow);
    assert_eq!(
        compaction_notices(&rig),
        ["notice:compaction:started", "notice:compaction:failed"]
    );
    let notices = rig.events.notices();
    let Some(Notice::Compaction(failed)) = notices.last() else {
        panic!("{notices:#?}");
    };
    assert!(!failed.background);
}
