//! The plan, through the loop, with the real `plan` tool
//! (`docs/architecture/05-tools.md`, "`plan`";
//! `docs/architecture/03-agent-loop.md`, "Steering and queued prompts").
//!
//! The rest of this crate's tests use fake tools. These use the real one,
//! because what they show is the loop and the tool together: a user's edit
//! reaches the model as a notice, the next tool result returns the current
//! plan, and the loop's round numbers are what tells the tool whether the
//! model has seen that result.

use std::sync::Arc;

use cortex_protocol::ids::PlanItemId;
use cortex_protocol::item::{NoticeSource, ToolOutcome, ToolResultItem};
use cortex_protocol::project::{PlanAdd, PlanEdit, PlanItemRef, PlanReword};
use cortex_protocol::{Item, Part, StopReason};
use cortex_testkit::{Gate, Response};
use cortex_tools::{PlanBoard, PlanTool};
use serde_json::{Value, json};

use crate::sessions::Flaky;
use crate::transitions::{retry, start};
use crate::{Rig, Setup, World, assert_valid_history, calls, last_request, lock, says};

fn items(list: &[(&str, &str, &str)]) -> Value {
    json!({"items": list
        .iter()
        .map(|(id, text, status)| json!({"id": id, "text": text, "status": status}))
        .collect::<Vec<_>>()})
}

fn ids(board: &PlanBoard) -> Vec<String> {
    board
        .get()
        .items
        .iter()
        .map(|item| item.id.to_string())
        .collect()
}

/// Everything a request holds, as one text to look into.
fn sent(world: &World) -> String {
    let request = last_request(&world.provider);
    assert_valid_history(&request.messages);
    format!("{:?}", request.messages)
}

/// The result recorded for a call, with its text.
fn result_of(rig: &Rig, call: &str) -> (ToolResultItem, String) {
    let result = rig
        .store
        .items()
        .into_iter()
        .find_map(|item| match item {
            Item::ToolResult(result) if result.call_id.as_str() == call => Some(result),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no result for `{call}`"));
    let text = result
        .content
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    (result, text)
}

fn has_result(rig: &Rig, call: &str) -> bool {
    rig.store
        .items()
        .iter()
        .any(|item| matches!(item, Item::ToolResult(result) if result.call_id.as_str() == call))
}

/// A session whose tools are the fake ones and the real `plan`.
fn with_plan(world: &World, mut setup: Setup) -> (Arc<PlanBoard>, Rig) {
    let board = Arc::new(PlanBoard::new());
    setup.toolbox = Some(world.toolbox().with(PlanTool::new(Arc::clone(&board))));
    (board, setup.start(world))
}

fn first() -> Value {
    items(&[
        ("read", "Read the parser", "active"),
        ("fix", "Fix the bug", "pending"),
        ("test", "Add a test", "pending"),
    ])
}

/// Written by the model before it has seen what the user did: the item the
/// user removes is still in it.
fn stale() -> Value {
    items(&[
        ("read", "Read the parser", "done"),
        ("fix", "Fix the bug", "active"),
        ("test", "Add a test", "pending"),
    ])
}

/// The user's edit: an item removed and one added. What it returns is the
/// notice for the model.
fn remove_test_and_add_one(board: &PlanBoard) -> String {
    board
        .apply(&[
            PlanEdit::Remove(PlanItemRef {
                id: PlanItemId::new("test"),
            }),
            PlanEdit::Add(PlanAdd {
                text: "Ask before changing the lexer".to_owned(),
                after: None,
            }),
        ])
        .expect("the edits fit the plan")
        .notice
        .expect("there is something to tell the model")
}

#[test]
fn a_users_edit_reaches_the_model_as_a_notice_and_the_next_result_returns_the_current_plan() {
    let more = Gate::new();
    // Two calls in one response, both written without the user's edit.
    let second = Response::new()
        .text("Updating the plan.")
        .tool_call("c2", "plan", &stale())
        .tool_call("c3", "plan", &items(&[("test", "Add a test", "active")]))
        .wait(&more)
        .end(StopReason::ToolCalls);
    // Written after both results and the notice: the model's own choice.
    let informed = items(&[
        ("read", "Read the parser", "done"),
        ("u1", "Ask before changing the lexer", "active"),
    ]);
    let world = World::new(
        "plan_user_edit",
        [
            calls(&[("c1", "plan", first())]),
            second,
            calls(&[("c4", "plan", informed)]),
            says("Understood."),
        ],
    );
    let (board, rig) = with_plan(&world, Setup::new(&world));

    world.run(async {
        rig.prompt("go");
        // Both calls of the second response have been planned, and the
        // model is still writing. They change the session, so neither has
        // run: the plan is the first list.
        rig.policy.decisions(3).await;
        assert_eq!(board.get().revision, 1);
        assert_eq!(ids(&board), ["read", "fix", "test"]);

        // The user edits the plan in the panel. It changes at once, and
        // the core hands the loop what to tell the model.
        let notice = remove_test_and_add_one(&board);
        assert_eq!(ids(&board), ["read", "fix", "u1"]);
        rig.handle.notice(NoticeSource::Plan, notice);
        more.open();
        rig.events.turns(1).await;
    });

    // What each of the two calls was told: what the user changed, and the
    // current plan. Neither replaced anything: the second was written
    // beside the first, not after its result.
    for call in ["c2", "c3"] {
        let (result, text) = result_of(&rig, call);
        assert_eq!(result.outcome, ToolOutcome::Error, "{call}: {text}");
        assert!(text.contains("The plan was not replaced"), "{text}");
        assert!(text.contains("removed `test` (Add a test)"), "{text}");
        assert!(
            text.contains("3. [pending] u1: Ask before changing the lexer"),
            "{text}"
        );
    }
    // The notice reached the model too, after the round and not inside it.
    let log = rig.store.items();
    let position = |wanted: &dyn Fn(&Item) -> bool| {
        log.iter().position(wanted).expect("the item is in the log")
    };
    let refused = position(
        &|item| matches!(item, Item::ToolResult(result) if result.call_id.as_str() == "c3"),
    );
    let notice = position(
        &|item| matches!(item, Item::Notice(notice) if notice.source == NoticeSource::Plan),
    );
    assert!(refused < notice);
    let sent = sent(&world);
    assert!(sent.contains("The user edited the plan:"), "{sent}");
    assert!(
        sent.contains("added `u1` at the end: Ask before changing the lexer"),
        "{sent}"
    );

    // The next response was written with all of that in view, and its
    // call is the plan.
    let (result, text) = result_of(&rig, "c4");
    assert_eq!(result.outcome, ToolOutcome::Success, "{text}");
    assert_eq!(ids(&board), ["read", "u1"]);
}

#[test]
fn a_notice_delivered_while_idle_is_followed_by_a_call_that_goes_through() {
    let first = items(&[("a", "First", "active"), ("b", "Second", "pending")]);
    // The model has been told of the user's edit, and its next list is
    // written with it: the plan as the user left it, moved on.
    let informed = items(&[("a", "First", "done"), ("b", "Second, carefully", "active")]);
    // The response is held until the test has played the core's part.
    let held = Gate::new();
    let next = Response::new()
        .wait(&held)
        .tool_call("c2", "plan", &informed)
        .end(StopReason::ToolCalls);
    let world = World::new(
        "plan_idle_edit",
        [
            calls(&[("c1", "plan", first)]),
            says("Planned."),
            next,
            says("Done."),
        ],
    );
    let (board, rig) = with_plan(&world, Setup::new(&world));

    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    // The session is idle. The user rewords an item.
    let change = board
        .apply(&[PlanEdit::Reword(PlanReword {
            id: PlanItemId::new("b"),
            text: "Second, carefully".to_owned(),
        })])
        .unwrap();
    rig.handle.notice(
        NoticeSource::Plan,
        change.notice.clone().expect("something to tell"),
    );
    world.run(async {
        // A notice starts no turn. It goes out ahead of the next prompt.
        rig.prompt("carry on");
        rig.events.seen("note:plan", 1).await;
        // The core sees the notice recorded, and tells the board that the
        // model has now been shown the edit.
        board.delivered(change.plan.revision);
        held.open();
        rig.events.turns(2).await;
    });
    // The first call after the edit went through: no call was spent on
    // being told what the notice had already said.
    let (result, text) = result_of(&rig, "c2");
    assert_eq!(result.outcome, ToolOutcome::Success, "{text}");
    assert!(
        text.starts_with("Plan saved: 2 items (1 done, 1 active), revision 3."),
        "{text}"
    );
    let plan = board.get();
    assert_eq!(plan.items[1].text, "Second, carefully");
    assert_eq!(plan.items[1].status.as_str(), "active");
    let log = rig.store.items();
    let notice = log
        .iter()
        .position(|item| matches!(item, Item::Notice(_)))
        .expect("the notice is in the log");
    assert!(
        matches!(log[notice + 1], Item::UserMessage(_)),
        "the notice is delivered ahead of the prompt that followed it"
    );
}

/// A round's number moves on only when its results have been recorded,
/// which is when the next request carries them. Here the result of a
/// refused call cannot be written to the log, so the model never sees it:
/// the next call must not be taken for one written with it in view.
#[test]
fn a_refusal_the_model_never_saw_does_not_count_as_shown() {
    let world = World::new(
        "plan_result_lost",
        [
            calls(&[("c1", "plan", first())]),
            says("Planned."),
            // The result of this call is lost with the turn.
            calls(&[("c2", "plan", stale())]),
            // So this one is, to the model, the first after the edit.
            calls(&[("c3", "plan", stale())]),
            // And this one is written with c3's result in view.
            calls(&[("c4", "plan", stale())]),
            says("Done."),
        ],
    );
    let store = Flaky::default();
    let mut setup = Setup::new(&world);
    setup.memory = store.inner.clone();
    setup.store = Some(Box::new(store.clone()));
    let (board, rig) = with_plan(&world, setup);

    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    remove_test_and_add_one(&board);

    // The prompt and the assistant's message are appended. The result of
    // its call is not.
    let appended = *lock(&store.appends);
    *lock(&store.fail_append) = Some(appended + 2);
    world.run(async {
        rig.prompt("go on");
        rig.events.turns(2).await;
    });
    assert_eq!(
        rig.events.ends()[1].error.as_ref().unwrap().code,
        "store_failed"
    );
    assert!(!has_result(&rig, "c2"), "the result is not in the log");
    assert_eq!(ids(&board), ["read", "fix", "u1"], "the call was refused");

    world.run(async {
        rig.prompt("again");
        rig.events.turns(3).await;
    });
    // The first call of this turn was refused too: the refusal before it
    // never reached the model.
    let (c3, text) = result_of(&rig, "c3");
    assert_eq!(c3.outcome, ToolOutcome::Error, "{text}");
    assert!(text.contains("The plan was not replaced"), "{text}");
    // Its result was recorded, so the call after it is the model's choice.
    let (c4, text) = result_of(&rig, "c4");
    assert_eq!(c4.outcome, ToolOutcome::Success, "{text}");
    assert_eq!(ids(&board), ["read", "fix", "test"]);
}

/// A request that fails part-way and is sent again is one round, with one
/// number: the calls of the response that counts are the first the model
/// made after the edit, however many attempts the request took.
#[test]
fn a_request_that_is_sent_again_is_still_the_same_round() {
    let fails = Gate::new();
    let retried = Response::new()
        .tool_call("c2", "plan", &stale())
        .wait(&fails)
        .event(retry(2))
        .event(start())
        .tool_call("c3", "plan", &stale())
        .tool_call("c4", "plan", &items(&[("test", "Add a test", "done")]))
        .end(StopReason::ToolCalls);
    let world = World::new(
        "plan_retried",
        [calls(&[("c1", "plan", first())]), retried, says("Done.")],
    );
    let (board, rig) = with_plan(&world, Setup::new(&world));
    world.run(async {
        rig.prompt("go");
        rig.policy.decisions(2).await;
        remove_test_and_add_one(&board);
        fails.open();
        rig.events.turns(1).await;
    });
    // The call of the attempt that was thrown away never ran, and both
    // calls of the response that counts were refused.
    assert!(!has_result(&rig, "c2"));
    for call in ["c3", "c4"] {
        let (result, text) = result_of(&rig, call);
        assert_eq!(result.outcome, ToolOutcome::Error, "{call}: {text}");
    }
    assert_eq!(ids(&board), ["read", "fix", "u1"]);
}
