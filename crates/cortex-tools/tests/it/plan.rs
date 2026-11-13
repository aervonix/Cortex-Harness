//! Contract tests for `plan` and the plan a user can edit: every row of
//! the tool's section in `docs/architecture/05-tools.md`.

use std::sync::Arc;

use cortex_protocol::ids::PlanItemId;
use cortex_protocol::item::{PlanStatus, ToolOutcome};
use cortex_protocol::project::{PlanAdd, PlanEdit, PlanItemRef, PlanReorder, PlanReword};
use cortex_tools::{Concurrency, PlanBoard, PlanEditError, PlanTool};
use serde_json::{Value, json};

use crate::{Workspace, failed, ok};

fn item(id: &str, text: &str, status: &str) -> Value {
    json!({"id": id, "text": text, "status": status})
}

fn three() -> Value {
    json!({"items": [
        item("read", "Read the parser", "done"),
        item("fix", "Fix the off-by-one", "active"),
        item("test", "Add a test", "pending"),
    ]})
}

fn id(text: &str) -> PlanItemId {
    PlanItemId::new(text)
}

fn named(text: &str) -> PlanItemRef {
    PlanItemRef { id: id(text) }
}

/// The ids of the plan, in order, each with its status.
fn shape(board: &PlanBoard) -> Vec<String> {
    board
        .get()
        .items
        .iter()
        .map(|item| format!("{}:{}", item.id, item.status.as_str()))
        .collect()
}

#[test]
fn each_call_replaces_the_whole_list() {
    let ws = Workspace::new("plan_replace");
    let board = Arc::new(PlanBoard::new());
    let tool = PlanTool::new(Arc::clone(&board));
    assert!(board.get().items.is_empty());

    let result = ws.call(&tool, three());
    assert_eq!(
        ok(&result),
        "Plan saved: 3 items (1 done, 1 active, 1 pending), revision 1. It is exactly the list \
         you sent."
    );
    assert_eq!(shape(&board), ["read:done", "fix:active", "test:pending"]);
    assert_eq!(board.get().revision, 1);
    // Front ends get the plan itself.
    let payload = result.payload().expect("a payload for front ends");
    assert_eq!(payload["replaced"], true);
    assert_eq!(payload["plan"]["revision"], 1);
    assert_eq!(payload["plan"]["items"][1]["text"], "Fix the off-by-one");

    // The next call is the whole plan again: what it leaves out is gone.
    let result = ws.call(
        &tool,
        json!({"items": [
            item("fix", "Fix the off-by-one", "done"),
            item("docs", "Update the docs", "skipped"),
        ]}),
    );
    assert_eq!(
        ok(&result),
        "Plan saved: 2 items (1 done, 1 skipped), revision 2. It is exactly the list you sent."
    );
    assert_eq!(shape(&board), ["fix:done", "docs:skipped"]);

    // The same list again changes nothing, and the revision stays.
    let result = ws.call(
        &tool,
        json!({"items": [
            item("fix", "Fix the off-by-one", "done"),
            item("docs", "Update the docs", "skipped"),
        ]}),
    );
    assert!(ok(&result).starts_with("Plan unchanged: 2 items"));
    assert_eq!(board.get().revision, 2);

    // An empty list clears the plan.
    let result = ws.call(&tool, json!({"items": []}));
    assert_eq!(
        ok(&result),
        "Plan saved: no items, revision 3. It is exactly the list you sent."
    );
    assert!(board.get().items.is_empty());
}

#[test]
fn the_call_touches_nothing_outside_the_session_and_says_that_it_changes_the_session() {
    let ws = Workspace::new("plan_plan");
    let board = Arc::new(PlanBoard::new());
    let tool = PlanTool::new(Arc::clone(&board));
    let plan = ws.plan(&tool, three()).unwrap();
    assert!(plan.effects.is_empty(), "nothing for the permission engine");
    assert_eq!(plan.concurrency, Concurrency::ReadOnly);
    assert!(!plan.has_side_effects());
    // The marker: the loop must not start it before the response has
    // ended, or run two of them at once.
    assert!(plan.changes_session());
    assert!(plan.conflicts_with(&ws.plan(&tool, three()).unwrap()));
    assert_eq!(plan.summary, "Update the plan (3 items)");
    assert!(board.get().items.is_empty(), "planning changed nothing");
}

#[test]
fn a_list_that_is_not_a_plan_is_refused_when_the_call_is_planned() {
    let ws = Workspace::new("plan_arguments");
    let board = Arc::new(PlanBoard::new());
    let tool = PlanTool::new(Arc::clone(&board));
    let refused = |args: Value| failed(&ws.call(&tool, args));
    assert!(refused(json!({})).contains("items"));
    assert!(
        refused(json!({"items": [item("a", "One", "started")]})).contains("status"),
        "a status that is not one of the four"
    );
    assert_eq!(
        refused(json!({"items": [item("a", "One", "done"), item("a", "Two", "pending")]})),
        "Item 2: the id `a` is used twice. Every item needs an id of its own."
    );
    assert!(refused(json!({"items": [item("two words", "One", "done")]})).contains("`id`"));
    assert!(refused(json!({"items": [item("a", "  ", "done")]})).contains("text is empty"));
    assert!(
        refused(json!({"items": [item("a", "one\ntwo", "done")]})).contains("one line"),
        "an item is one line"
    );
    assert!(
        refused(json!({"items": [item("a", &"long ".repeat(100), "done")]}))
            .contains("at most 400 characters")
    );
    assert!(board.get().items.is_empty());
    assert_eq!(board.get().revision, 0);
}

#[test]
fn the_user_can_reorder_skip_reword_add_and_remove() {
    let ws = Workspace::new("plan_user_edits");
    let board = Arc::new(PlanBoard::new());
    ok(&ws.call(&PlanTool::new(Arc::clone(&board)), three()));

    let change = board
        .apply(&[
            PlanEdit::Skip(named("test")),
            PlanEdit::Reword(PlanReword {
                id: id("fix"),
                text: "Fix the off-by-one in the lexer".to_owned(),
            }),
            PlanEdit::Add(PlanAdd {
                text: "Run it on Windows".to_owned(),
                after: Some(id("fix")),
            }),
            PlanEdit::Add(PlanAdd {
                text: "Tell the team".to_owned(),
                after: None,
            }),
            PlanEdit::Remove(named("read")),
            PlanEdit::Reorder(PlanReorder {
                order: vec![id("u1"), id("fix"), id("u2"), id("test")],
            }),
        ])
        .unwrap();
    // The stored plan changed at once, in one revision.
    assert_eq!(
        shape(&board),
        ["u1:pending", "fix:active", "u2:pending", "test:skipped"]
    );
    assert_eq!(board.get().revision, 2);
    assert_eq!(change.plan, board.get());
    // The notice says exactly what changed, and gives the plan as it is.
    assert_eq!(
        change.notice.as_deref(),
        Some(
            "The user edited the plan:\n\
             - skipped `test` (Add a test)\n\
             - reworded `fix`: it was \"Fix the off-by-one\" and is now \"Fix the off-by-one in \
             the lexer\"\n\
             - added `u1` after `fix`: Run it on Windows\n\
             - added `u2` at the end: Tell the team\n\
             - removed `read` (Read the parser)\n\
             - changed the order to `u1`, `fix`, `u2`, `test`\n\
             The plan is now (revision 2):\n\
             1. [pending] u1: Run it on Windows\n\
             2. [active] fix: Fix the off-by-one in the lexer\n\
             3. [pending] u2: Tell the team\n\
             4. [skipped] test: Add a test\n\
             Work from this plan. A `plan` call replaces the whole list, so base your next one \
             on it."
        )
    );
}

#[test]
fn an_edit_that_does_not_fit_changes_nothing_and_one_that_changes_nothing_tells_nobody() {
    let ws = Workspace::new("plan_bad_edits");
    let board = Arc::new(PlanBoard::new());
    ok(&ws.call(&PlanTool::new(Arc::clone(&board)), three()));
    let before = board.get();

    // The first edit of each would apply. None of it does.
    let good = PlanEdit::Skip(named("test"));
    for (edit, wanted) in [
        (
            PlanEdit::Remove(named("gone")),
            PlanEditError::UnknownItem("gone".to_owned()),
        ),
        (
            PlanEdit::Reorder(PlanReorder {
                order: vec![id("read"), id("fix")],
            }),
            PlanEditError::NotAnOrdering,
        ),
        (
            PlanEdit::Reorder(PlanReorder {
                order: vec![id("read"), id("fix"), id("fix")],
            }),
            PlanEditError::NotAnOrdering,
        ),
        (
            PlanEdit::Reorder(PlanReorder {
                order: vec![id("read"), id("fix"), id("other")],
            }),
            PlanEditError::NotAnOrdering,
        ),
        (
            PlanEdit::Add(PlanAdd {
                text: "x".to_owned(),
                after: Some(id("gone")),
            }),
            PlanEditError::UnknownItem("gone".to_owned()),
        ),
        (
            PlanEdit::Reword(PlanReword {
                id: id("fix"),
                text: "   ".to_owned(),
            }),
            PlanEditError::BadText("an item's text is empty".to_owned()),
        ),
    ] {
        assert_eq!(board.apply(&[good.clone(), edit]), Err(wanted));
        assert_eq!(board.get(), before, "all or none");
    }
    assert_eq!(
        PlanEditError::UnknownItem("gone".to_owned()).to_string(),
        "the plan has no item `gone`"
    );

    // Edits that leave the plan as it is: no new revision, no notice.
    let change = board
        .apply(&[
            PlanEdit::Reorder(PlanReorder {
                order: vec![id("read"), id("fix"), id("test")],
            }),
            PlanEdit::Reword(PlanReword {
                id: id("fix"),
                text: "Fix the off-by-one".to_owned(),
            }),
        ])
        .unwrap();
    assert_eq!(change.notice, None);
    assert_eq!(board.get(), before);

    // After such a batch the model's next call is not held up.
    let tool = PlanTool::new(Arc::clone(&board));
    ok(&ws.call(
        &tool,
        json!({"items": [item("read", "Read the parser", "done")]}),
    ));
}

/// The done criterion: a user's edit reaches the model as a notice, and
/// the next tool result returns the current plan. The edit also wins over
/// a call the model wrote without it.
#[test]
fn a_call_that_crosses_a_users_edit_replaces_nothing_and_returns_the_current_plan() {
    let ws = Workspace::new("plan_crossing");
    let board = Arc::new(PlanBoard::new());
    let tool = PlanTool::new(Arc::clone(&board));
    ok(&ws.call(&tool, three()));

    // The user removes an item and adds one, while the model works.
    let change = board
        .apply(&[
            PlanEdit::Remove(named("test")),
            PlanEdit::Add(PlanAdd {
                text: "Ask before touching the lexer".to_owned(),
                after: None,
            }),
        ])
        .unwrap();
    assert!(
        change.notice.is_some(),
        "there is something to tell the model"
    );

    // The model's next call was written from the list it last saw: it
    // still has the removed item, and not the new one.
    let stale = json!({"items": [
        item("read", "Read the parser", "done"),
        item("fix", "Fix the off-by-one", "done"),
        item("test", "Add a test", "active"),
    ]});
    let result = ws.call(&tool, stale.clone());
    assert_eq!(result.outcome(), &ToolOutcome::Error);
    assert_eq!(
        result.text(),
        "The plan was not replaced: the user has edited it since you last saw it, and your \
         list was written without their changes. The user:\n\
         - removed `test` (Add a test)\n\
         - added `u1` at the end: Ask before touching the lexer\n\
         The plan is now (revision 2):\n\
         1. [done] read: Read the parser\n\
         2. [active] fix: Fix the off-by-one\n\
         3. [pending] u1: Ask before touching the lexer\n\
         Send your update again, based on this plan."
    );
    // The user's edit stands.
    assert_eq!(shape(&board), ["read:done", "fix:active", "u1:pending"]);
    assert_eq!(board.get().revision, 2);
    let payload = result.payload().unwrap();
    assert_eq!(payload["replaced"], false);
    assert_eq!(payload["plan"]["items"][2]["id"], "u1");
    assert_eq!(payload["user_edits"].as_array().unwrap().len(), 2);

    // The model now has the current plan, and its next call goes through,
    // whatever it holds: the choice is the model's again.
    let result = ws.call(
        &tool,
        json!({"items": [
            item("read", "Read the parser", "done"),
            item("fix", "Fix the off-by-one", "done"),
            item("u1", "Ask before touching the lexer", "active"),
        ]}),
    );
    ok(&result);
    assert_eq!(shape(&board), ["read:done", "fix:done", "u1:active"]);
    assert_eq!(board.get().revision, 3);

    // A call that already is the plan the user left crosses nothing.
    board.apply(&[PlanEdit::Skip(named("u1"))]).unwrap();
    let result = ws.call(
        &tool,
        json!({"items": [
            item("read", "Read the parser", "done"),
            item("fix", "Fix the off-by-one", "done"),
            item("u1", "Ask before touching the lexer", "skipped"),
        ]}),
    );
    assert!(ok(&result).starts_with("Plan unchanged"));
}

/// The model is also shown an edit by its notice. Once the core says the
/// notice was delivered, a call written with it goes through at once.
#[test]
fn an_edit_whose_notice_was_delivered_does_not_hold_up_the_next_call() {
    let ws = Workspace::new("plan_delivered");
    let board = Arc::new(PlanBoard::new());
    let tool = PlanTool::new(Arc::clone(&board));
    ok(&ws.call(&tool, three()));

    let first = board.apply(&[PlanEdit::Skip(named("test"))]).unwrap();
    let second = board.apply(&[PlanEdit::Remove(named("read"))]).unwrap();
    assert_eq!((first.plan.revision, second.plan.revision), (2, 3));
    // Only the first notice has reached the model: the second edit is
    // still news, and a call crosses it.
    board.delivered(first.plan.revision);
    let moved_on = json!({"items": [
        item("fix", "Fix the off-by-one", "done"),
        item("test", "Add a test", "skipped"),
    ]});
    let result = ws.call(&tool, moved_on.clone());
    assert_eq!(result.outcome(), &ToolOutcome::Error);
    let text = result.text();
    assert!(
        text.contains("- removed `read` (Read the parser)"),
        "{text}"
    );
    assert!(!text.contains("skipped `test`"), "already told: {text}");

    // Both delivered: the model's list, written with them, is the plan.
    board.apply(&[PlanEdit::Skip(named("fix"))]).unwrap();
    board.delivered(board.get().revision);
    ok(&ws.call(&tool, moved_on));
    assert_eq!(shape(&board), ["fix:done", "test:skipped"]);
}

#[test]
fn a_resumed_session_starts_from_the_plan_it_had() {
    let ws = Workspace::new("plan_resume");
    let first = Arc::new(PlanBoard::new());
    ok(&ws.call(&PlanTool::new(Arc::clone(&first)), three()));
    let resumed = Arc::new(PlanBoard::with_plan(first.get()));
    assert_eq!(resumed.get(), first.get());
    assert_eq!(resumed.get().items[0].status, PlanStatus::Done);
    // And goes on counting revisions from there.
    ok(&ws.call(&PlanTool::new(Arc::clone(&resumed)), json!({"items": []})));
    assert_eq!(resumed.get().revision, 2);
}

#[test]
fn a_call_cancelled_before_it_starts_leaves_the_plan() {
    let ws = Workspace::new("plan_cancelled");
    let board = Arc::new(PlanBoard::new());
    let cancel = cortex_base::CancelToken::new();
    cancel.cancel();
    let result = ws.call_with(
        &ws.cx().with_cancel(cancel),
        &PlanTool::new(Arc::clone(&board)),
        three(),
    );
    assert_eq!(result.outcome(), &ToolOutcome::Interrupted);
    assert!(board.get().items.is_empty());
}
