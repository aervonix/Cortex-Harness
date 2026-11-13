//! The plan, through the loop, with the real `plan` tool
//! (`docs/architecture/05-tools.md`, "`plan`";
//! `docs/architecture/03-agent-loop.md`, "Steering and queued prompts").
//!
//! The rest of this crate's tests use fake tools. These use the real one,
//! because what they show is the loop and the tool together: a user's edit
//! reaches the model as a notice, and the next tool result returns the
//! current plan.

use std::sync::Arc;

use cortex_protocol::ids::PlanItemId;
use cortex_protocol::item::NoticeSource;
use cortex_protocol::project::{PlanAdd, PlanEdit, PlanItemRef, PlanReword};
use cortex_protocol::{Item, StopReason};
use cortex_testkit::{Gate, Response};
use cortex_tools::{PlanBoard, PlanTool};
use serde_json::{Value, json};

use crate::{Setup, World, assert_valid_history, calls, last_request, says};

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

#[test]
fn a_users_edit_reaches_the_model_as_a_notice_and_the_next_result_returns_the_current_plan() {
    let first = items(&[
        ("read", "Read the parser", "active"),
        ("fix", "Fix the bug", "pending"),
        ("test", "Add a test", "pending"),
    ]);
    // Written by the model before it has seen what the user did: the item
    // the user removed is still in it.
    let stale = items(&[
        ("read", "Read the parser", "done"),
        ("fix", "Fix the bug", "active"),
        ("test", "Add a test", "pending"),
    ]);
    let more = Gate::new();
    let second = Response::new()
        .text("Updating the plan.")
        .tool_call("c2", "plan", &stale)
        .wait(&more)
        .end(StopReason::ToolCalls);
    let world = World::new(
        "plan_user_edit",
        [calls(&[("c1", "plan", first)]), second, says("Understood.")],
    );
    let board = Arc::new(PlanBoard::new());
    let mut setup = Setup::new(&world);
    setup.toolbox = Some(world.toolbox().with(PlanTool::new(Arc::clone(&board))));
    let rig = setup.start(&world);

    world.run(async {
        rig.prompt("go");
        // The second call has been planned, and the model is still
        // writing. The call changes the session, so it has not run: the
        // plan is the first list.
        rig.policy.decisions(2).await;
        assert_eq!(board.get().revision, 1);
        assert_eq!(ids(&board), ["read", "fix", "test"]);

        // The user edits the plan in the panel. It changes at once, and
        // the core hands the loop what to tell the model.
        let change = board
            .apply(&[
                PlanEdit::Remove(PlanItemRef {
                    id: PlanItemId::new("test"),
                }),
                PlanEdit::Add(PlanAdd {
                    text: "Ask before changing the lexer".to_owned(),
                    after: None,
                }),
            ])
            .expect("the edits fit the plan");
        assert_eq!(ids(&board), ["read", "fix", "u1"]);
        rig.handle.notice(
            NoticeSource::Plan,
            change.notice.expect("there is something to tell the model"),
        );
        more.open();
        rig.events.turns(1).await;
    });

    // The user's edit stands: the call that crossed it replaced nothing.
    assert_eq!(ids(&board), ["read", "fix", "u1"]);
    assert_eq!(board.get().items[0].status.as_str(), "active");

    // What the model was sent next: the result of its call, which says
    // what the user changed and gives the current plan, and the notice.
    let sent = sent(&world);
    assert!(sent.contains("The plan was not replaced"), "{sent}");
    assert!(sent.contains("removed `test` (Add a test)"), "{sent}");
    assert!(
        sent.contains("3. [pending] u1: Ask before changing the lexer"),
        "{sent}"
    );
    assert!(sent.contains("The user edited the plan:"), "{sent}");
    assert!(
        sent.contains("added `u1` at the end: Ask before changing the lexer"),
        "{sent}"
    );
    // In the log, the result comes before the notice: a notice is appended
    // at a safe boundary, never inside a tool round.
    let log = rig.store.items();
    let result = log
        .iter()
        .rposition(|item| matches!(item, Item::ToolResult(_)))
        .expect("the call has its result");
    let notice = log
        .iter()
        .position(
            |item| matches!(item, Item::Notice(notice) if notice.source == NoticeSource::Plan),
        )
        .expect("the notice is in the log");
    assert!(result < notice);
}

#[test]
fn a_notice_delivered_while_idle_is_followed_by_a_call_that_goes_through() {
    let first = items(&[("a", "First", "active"), ("b", "Second", "pending")]);
    // The model has been told of the user's edit, and its next list is
    // written with it: the plan as the user left it, moved on.
    let informed = items(&[("a", "First", "done"), ("b", "Second, carefully", "active")]);
    let world = World::new(
        "plan_idle_edit",
        [
            calls(&[("c1", "plan", first)]),
            says("Planned."),
            calls(&[("c2", "plan", informed)]),
            says("Done."),
        ],
    );
    let board = Arc::new(PlanBoard::new());
    let mut setup = Setup::new(&world);
    setup.toolbox = Some(world.toolbox().with(PlanTool::new(Arc::clone(&board))));
    let rig = setup.start(&world);

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
        change.notice.expect("something to tell"),
    );
    world.run(async {
        // A notice starts no turn. It goes out ahead of the next prompt.
        rig.prompt("carry on");
        rig.events.seen("note:plan", 1).await;
        // The core sees the notice recorded, and tells the board that the
        // model has now been shown the edit.
        board.delivered(change.plan.revision);
        rig.events.turns(2).await;
    });
    let plan = board.get();
    assert_eq!(plan.items[0].status.as_str(), "done");
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
    assert!(sent(&world).contains("Plan saved: 2 items (1 done, 1 active), revision 3."));
}
