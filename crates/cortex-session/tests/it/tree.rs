//! The tree: resume, records, repair, rewind, switch, fork, leases.

use std::fs;

use cortex_protocol::error::code;
use cortex_protocol::event::{TurnCompleted, TurnOutcome};
use cortex_protocol::ids::{CallId, ItemId, SessionId, TurnId};
use cortex_protocol::item::{Item, ToolOutcome};
use cortex_protocol::lease::Lease;
use cortex_session::entry::{
    Body, EntryId, Kind, LeaseEvent, LeaseRecord, Meta, Outcome, Reverted, RewindReason,
};
use cortex_session::{Log, Restore};
use serde_json::json;

use crate::{World, assistant, tool_result, user, user_item};

#[test]
fn a_resumed_session_continues_where_it_left_off() {
    let world = World::new("resume_continues");
    let mut session = world.create("s_1");
    let turn = session.next_turn_id();
    assert_eq!(turn.as_str(), "t_1");
    session
        .append_item(Some(&turn), user_item("i_1", "hello"))
        .unwrap();
    session
        .append(Some(&turn), assistant("i_2", "hi", &[]))
        .unwrap();
    session.end_turn().unwrap();
    let branch = session.branch();
    drop(session);

    let (mut session, report) = world.resume("s_1");
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(session.branch(), branch);
    assert_eq!(session.last_ids(), (1, 2));
    assert_eq!(session.next_turn_id().as_str(), "t_2");
    assert_eq!(session.next_item_id().as_str(), "i_3");
    let next = session.append(None, user("again")).unwrap();
    assert_eq!(next.as_str(), "e_0003");
    let items = session.items().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].id(), Some("i_1"));
}

#[test]
fn dangling_tool_calls_are_repaired_on_resume_and_never_run_again() {
    let world = World::new("dangling_calls");
    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("look")).unwrap();
    session
        .append(
            Some(&turn),
            assistant("i_2", "Looking.", &["c_1", "c_2", "c_3"]),
        )
        .unwrap();
    session
        .append(Some(&turn), tool_result("i_3", "c_2"))
        .unwrap();
    // A checkpoint after a result does not hide the round.
    session
        .append(
            Some(&turn),
            Body::Checkpoint(cortex_session::entry::Checkpoint::default()),
        )
        .unwrap();
    drop(session); // a crash during the round

    let (session, report) = world.resume("s_1");
    assert_eq!(
        report.repaired_calls,
        vec![CallId::new("c_1"), CallId::new("c_3")]
    );
    let items = session.items().unwrap();
    let repaired: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            Item::ToolResult(result) if result.outcome == ToolOutcome::Interrupted => Some((
                result.call_id.clone(),
                result.id.clone(),
                result.tool.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        repaired,
        vec![
            (CallId::new("c_1"), ItemId::new("i_4"), "read".to_owned()),
            (CallId::new("c_3"), ItemId::new("i_5"), "read".to_owned())
        ]
    );
    let last = session.read(session.active().unwrap()).unwrap();
    assert_eq!(last.turn, Some(turn));
    drop(session);
    let (_, report) = world.resume("s_1");
    assert!(report.repaired_calls.is_empty(), "repaired once only");
}

#[test]
fn records_stay_outside_the_conversation() {
    let world = World::new("records");
    let mut session = world.create("s_1");
    let prompt = session.append(None, user("hi")).unwrap();
    let meta = session
        .append(
            None,
            Body::Meta(Meta {
                title: Some("Greeting".into()),
                ..Meta::default()
            }),
        )
        .unwrap();
    let completed: TurnCompleted = serde_json::from_value(json!({"outcome": "completed"})).unwrap();
    let outcome = session
        .append(
            Some(&TurnId::new("t_1")),
            Body::Outcome(Outcome::completed(&completed)),
        )
        .unwrap();
    assert!(session.is_record(&meta) && session.is_record(&outcome));
    assert_eq!(session.active(), Some(&prompt));
    assert_eq!(session.branch().len(), 2);
    let answer = session
        .append(None, assistant("i_2", "hello", &[]))
        .unwrap();
    assert_eq!(session.parent_of(&answer), Some(&prompt));
    let outcomes = session.records(&Kind::Outcome).unwrap();
    assert!(
        matches!(&outcomes[0].body, Body::Outcome(o) if o.ended == Some(TurnOutcome::Completed))
    );
    assert_eq!(session.title().unwrap().as_deref(), Some("Greeting"));
}

#[test]
fn a_rewind_starts_a_sibling_branch_and_keeps_everything() {
    let world = World::new("rewind_branches");
    let mut session = world.create("s_1");
    let t1 = TurnId::new("t_1");
    let t2 = TurnId::new("t_2");
    let first = session.append(Some(&t1), user("one")).unwrap();
    let reply = session
        .append(Some(&t1), assistant("i_2", "1", &[]))
        .unwrap();
    session.append(Some(&t2), user("two")).unwrap();
    let old_leaf = session
        .append(Some(&t2), assistant("i_4", "2", &[]))
        .unwrap();

    let plan = session.plan_rewind(&reply).unwrap();
    assert_eq!(plan.turns, vec![t2.clone()]);
    assert!(plan.files.is_empty());
    let done = session
        .rewind(&plan, &Restore::conversation_only())
        .unwrap();
    assert_eq!(session.active(), Some(&done.entry));
    assert_eq!(session.parent_of(&done.entry), Some(&reply));
    // Nothing was removed.
    assert!(session.contains(&old_leaf));
    let sibling = session.append(None, user("two, again")).unwrap();
    assert_eq!(
        session
            .branch()
            .iter()
            .filter(|id| session.kind_of(id) == Some(&Kind::User))
            .count(),
        2
    );
    let leaves = session.leaves();
    assert!(
        leaves.contains(&old_leaf) && leaves.contains(&sibling),
        "{leaves:?}"
    );
    let outcomes = session.records(&Kind::Outcome).unwrap();
    assert!(outcomes.iter().any(|entry| entry.turn.as_ref() == Some(&t2)
        && matches!(&entry.body, Body::Outcome(o) if o.reverted == Some(Reverted::Rewound))));

    // Switching back, and the position survives a reopen.
    let switched = session.switch(&old_leaf).unwrap();
    assert_eq!(session.parent_of(&switched), Some(&old_leaf));
    drop(session);
    let (log, _) = Log::open(&world.project.session(&SessionId::new("s_1")).unwrap()).unwrap();
    assert_eq!(log.active(), Some(&switched));
    assert!(log.branch().contains(&old_leaf));
    assert!(!log.branch().contains(&sibling));
    assert!(log.branch().contains(&first));
    let marker = log.read(&switched).unwrap();
    assert!(matches!(marker.body, Body::Rewind(r) if r.reason == RewindReason::Switch));
}

#[test]
fn a_rewind_into_a_tool_round_answers_the_open_calls() {
    let world = World::new("rewind_into_round");
    let mut session = world.create("s_1");
    session.append(None, user("go")).unwrap();
    let asked = session
        .append(None, assistant("i_2", "", &["c_1"]))
        .unwrap();
    session.append(None, tool_result("i_3", "c_1")).unwrap();
    let plan = session.plan_rewind(&asked).unwrap();
    let done = session
        .rewind(&plan, &Restore::conversation_only())
        .unwrap();
    assert_eq!(done.repaired_calls, vec![CallId::new("c_1")]);
}

#[test]
fn undo_rewinds_to_before_the_last_turn() {
    let world = World::new("undo_turn");
    let mut session = world.create("s_1");
    let t1 = TurnId::new("t_1");
    let t2 = TurnId::new("t_2");
    session.append(Some(&t1), user("one")).unwrap();
    let end_of_first = session
        .append(Some(&t1), assistant("i_2", "1", &[]))
        .unwrap();
    session.append(Some(&t2), user("two")).unwrap();
    session
        .append(Some(&t2), assistant("i_4", "2", &[]))
        .unwrap();
    let plan = session.plan_undo().unwrap().unwrap();
    assert_eq!(plan.to, end_of_first);
    assert_eq!(plan.reason, RewindReason::Undo);
    session.rewind(&plan, &Restore::files()).unwrap();
    let records = session.records(&Kind::Outcome).unwrap();
    assert!(matches!(&records[0].body, Body::Outcome(o) if o.reverted == Some(Reverted::Undone)));
    assert_eq!(records[0].turn, Some(t2));
    // A plan made before the session moved on is refused.
    let stale = plan;
    assert!(session.rewind(&stale, &Restore::files()).is_err());
}

#[test]
fn a_fork_copies_the_branch_and_the_base_and_leaves_the_original() {
    let world = World::new("fork");
    let mut session = world.create("s_1");
    session.append(None, user("one")).unwrap();
    let keep = session.append(None, assistant("i_2", "1", &[])).unwrap();
    let artifact = session
        .store()
        .put_artifact("output", "text/plain", b"full output")
        .unwrap();
    session
        .append(
            None,
            Body::Base(cortex_session::BaseState {
                complete: true,
                ..Default::default()
            }),
        )
        .unwrap();
    session
        .append(None, lease_body(json!("granted"), lease_json()))
        .unwrap();
    session.append(None, user("two")).unwrap();
    session.end_turn().unwrap();
    let original = fs::read(session.dir().log()).unwrap();

    let (fork, report) = session
        .fork(Some(&keep), SessionId::new("s_2"), world.options())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        fs::read(session.dir().log()).unwrap(),
        original,
        "the original is untouched"
    );
    let origin = fork.header().forked_from.clone().unwrap();
    assert_eq!(origin.session.as_str(), "s_1");
    assert_eq!(origin.entry, keep);
    assert_eq!(fork.active(), Some(&keep));
    assert_eq!(fork.items().unwrap().len(), 2);
    assert!(fork.base().unwrap().is_some());
    assert_eq!(
        fork.lease_in_force().unwrap(),
        None,
        "a lease is not inherited"
    );
    let mut read = String::new();
    std::io::Read::read_to_string(
        &mut fork.store().open_artifact(&artifact.id).unwrap(),
        &mut read,
    )
    .unwrap();
    assert_eq!(read, "full output");
    let listed = world.project.index().list().unwrap();
    assert!(listed.iter().any(|record| record.id.as_str() == "s_2"
        && record.parent.as_ref().map(SessionId::as_str) == Some("s_1")));
}

fn lease_json() -> serde_json::Value {
    json!({
        "id": "l_1",
        "grant": {"level": "supervised", "sandbox": {"read": {"all": true}, "write": {"roots": ["/w"]}, "network": "off"}},
        "expires": {"kind": "end_of_session"},
        "granted_by": {"kind": "user"}
    })
}

fn lease_body(event: serde_json::Value, lease: serde_json::Value) -> Body {
    Body::Lease(LeaseRecord {
        event: serde_json::from_value::<LeaseEvent>(event).unwrap(),
        lease: Lease::from_wire(lease).unwrap(),
    })
}

#[test]
fn the_lease_in_force_is_read_strictly() {
    let world = World::new("lease_strict");
    let mut session = world.create("s_1");
    session.append(None, user("hi")).unwrap();
    assert_eq!(session.lease_in_force().unwrap(), None);
    session
        .append(None, lease_body(json!("granted"), lease_json()))
        .unwrap();
    assert_eq!(
        session.lease_in_force().unwrap().unwrap().id.as_str(),
        "l_1"
    );
    session
        .append(None, lease_body(json!("ended"), lease_json()))
        .unwrap();
    assert_eq!(session.lease_in_force().unwrap(), None);
    session
        .append(None, lease_body(json!("granted"), lease_json()))
        .unwrap();
    drop(session);

    // A newer build records a restriction this build does not know.
    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    let mut wider = lease_json();
    wider["grant"]["sandbox"]["hosts"] = json!(["example.com"]);
    let mut content = fs::read(dir.log()).unwrap();
    content.extend(
        serde_json::to_vec(
            &json!({"v": 1, "id": "e_0009", "parent": null, "ts": "2026-11-12T10:00:00Z",
            "kind": "lease", "event": "extended", "lease": wider}),
        )
        .unwrap(),
    );
    content.push(b'\n');
    fs::write(dir.log(), content).unwrap();

    let (log, report) = Log::open(&dir).unwrap();
    assert!(
        report.is_clean(),
        "the rest of the session opens: {report:?}"
    );
    let refusal = log.lease_in_force().unwrap_err();
    assert_eq!(refusal.code, code::LEASE_NOT_UNDERSTOOD);
    assert!(refusal.message.contains("hosts"), "{}", refusal.message);
    assert_eq!(log.items().unwrap().len(), 1);
    // Appending a record that was not understood is refused too.
    let refused = log.read(&EntryId::new("e_0009")).unwrap().body;
    let (mut session, _) = world.resume("s_1");
    assert!(session.append(None, refused).is_err());
}
