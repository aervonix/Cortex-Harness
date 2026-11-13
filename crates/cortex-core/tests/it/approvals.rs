//! Approvals: a call that needs an answer is put to the front ends that
//! may give one, and what happens when none can
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Approvals";
//! `docs/architecture/11-rpc-protocol.md`, "Client roles";
//! `docs/architecture/10-headless.md`, "Approvals").
//!
//! The call that asks is a `write` to a continuous-integration workflow,
//! which is a protected path: at every level short of `unrestricted` it
//! needs an answer.

use std::time::Duration;

use cortex_core::{ClientInfo, Outgoing};
use cortex_protocol::event::{Event, Resolution, TurnOutcome};
use cortex_protocol::lease::Effect;
use cortex_protocol::methods::{
    ApprovalChoice, ApprovalDetail, ApprovalResponse, ClientRole, Op, OpResult,
    PermissionsRevokeParams, ServerResponse, SessionRef,
};
use cortex_protocol::session::SessionSettings;
use cortex_testkit::Response;
use serde_json::json;

use crate::{Front, World, calls, says, settle};

const PROTECTED: &str = ".github/workflows/ci.yml";

fn protected_write(id: &str) -> Response {
    calls(&[(
        id,
        "write",
        json!({"path": PROTECTED, "content": "on: push\n"}),
    )])
}

fn resolutions(front: &Front) -> Vec<Resolution> {
    front
        .events()
        .into_iter()
        .filter_map(|event| match event.event {
            Event::RequestResolved(resolved) => Some(resolved.resolution),
            _ => None,
        })
        .collect()
}

#[test]
fn a_call_that_needs_an_answer_is_put_to_the_user_and_runs_when_allowed() {
    let world = World::new("approve", [protected_write("c1"), says("Done.")]);
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("set up CI").await;
        let (id, request) = front.approval().await;
        assert_eq!(request.session_id, front.session);
        assert_eq!(request.tool, "write");
        assert_eq!(request.call_id.as_str(), "c1");
        // Exactly what would happen, and why it asks.
        let ApprovalDetail::Target(target) = &request.detail else {
            panic!("not a target: {:?}", request.detail);
        };
        assert_eq!(target.effect, Effect::Write);
        assert!(target.target.replace('\\', "/").ends_with(PROTECTED));
        assert!(!request.reasons.is_empty());
        assert!(request.choices.contains(&ApprovalChoice::AllowOnce));
        assert!(request.choices.contains(&ApprovalChoice::Deny));
        // Nothing ran while the question was open.
        assert_eq!(world.read(PROTECTED), None);

        front.answer(&id, ApprovalChoice::AllowOnce).unwrap();
        front.turns(1).await;
        assert_eq!(world.read(PROTECTED).as_deref(), Some("on: push\n"));
        assert_eq!(front.results()[0].1, "success");
        assert_eq!(resolutions(&front), [Resolution::Answered]);
        // Nobody was refused for want of somebody to ask.
        assert!(core.permission_denials(&front.session).is_empty());
        front.close().await;
    });
}

#[test]
fn a_denial_with_a_note_reaches_the_model_and_the_run_goes_on() {
    let world = World::new(
        "deny_note",
        [protected_write("c1"), says("I will not, then.")],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("set up CI").await;
        let (id, _) = front.approval().await;
        core.respond(
            front.client.id,
            &id,
            ServerResponse::Approval(ApprovalResponse {
                choice: ApprovalChoice::Deny,
                note: Some("not in this repository".to_owned()),
            }),
        )
        .unwrap();
        let end = {
            front.turns(1).await;
            front.ends().pop().unwrap()
        };
        assert_eq!(end.outcome, TurnOutcome::Completed);
        assert_eq!(world.read(PROTECTED), None);
        let results = front.results();
        assert_eq!(results[0].1, "denied");
        assert!(
            results[0].2.contains("not in this repository"),
            "{}",
            results[0].2
        );
        assert_eq!(front.last_text(), "I will not, then.");
        front.close().await;
    });
}

#[test]
fn only_a_user_client_is_asked_and_only_its_answer_counts() {
    let world = World::new("roles", [protected_write("c1"), says("Done.")]);
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let observer = front.join(ClientInfo::observer()).await;
        let supervisor = front
            .join(ClientInfo {
                role: ClientRole::Supervisor,
                ..ClientInfo::user()
            })
            .await;
        front.prompt("set up CI").await;
        let (id, _) = front.approval().await;
        settle().await;

        // Neither was sent the request, though both see the session.
        for other in [&observer, &supervisor] {
            other.drain();
            assert!(
                !other
                    .seen
                    .borrow()
                    .iter()
                    .any(|outgoing| matches!(outgoing, Outgoing::Request { .. })),
                "a client that may not answer was asked"
            );
            assert!(!other.events().is_empty(), "it watches the session");
            // And an answer from it is refused, and changes nothing.
            let refused = other.answer(&id, ApprovalChoice::AllowOnce).unwrap_err();
            assert_eq!(refused.code, "role_not_allowed");
        }
        settle().await;
        assert_eq!(world.read(PROTECTED), None, "a refused answer ran the call");
        assert!(front.ends().is_empty());

        // An observer cannot drive the session either.
        let refused = observer
            .op(Op::TurnInterrupt(
                cortex_protocol::methods::TurnInterruptParams::new(front.session.clone()),
            ))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "role_not_allowed");

        // The user's answer is the one that counts.
        front.answer(&id, ApprovalChoice::AllowOnce).unwrap();
        front.turns(1).await;
        assert_eq!(world.read(PROTECTED).as_deref(), Some("on: push\n"));
        front.close().await;
    });
}

#[test]
fn the_first_answer_wins_and_the_other_client_is_told() {
    let world = World::new("first_wins", [protected_write("c1"), says("Done.")]);
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let second = front.join(ClientInfo::user()).await;
        front.prompt("set up CI").await;
        let (id, _) = front.approval().await;
        let (same, _) = second.approval().await;
        assert_eq!(id, same, "one request, put to both");

        second.answer(&id, ApprovalChoice::Deny).unwrap();
        // The request is closed: a later answer is too late, and says so.
        let late = front.answer(&id, ApprovalChoice::AllowOnce).unwrap_err();
        assert_eq!(late.code, "request_not_open");
        front.turns(1).await;
        assert_eq!(world.read(PROTECTED), None, "the first answer was a denial");
        // Both are told that it was settled.
        second.turns(1).await;
        assert_eq!(resolutions(&front), [Resolution::Answered]);
        assert_eq!(resolutions(&second), [Resolution::Answered]);
        front.close().await;
        second.close().await;
    });
}

#[test]
fn with_nobody_to_ask_the_call_is_refused_recorded_and_the_run_goes_on() {
    let world = World::new(
        "nobody",
        [protected_write("c1"), says("I could not write it.")],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        let end = front.turn("set up CI").await;
        // The model is told, and carries on: the turn completes.
        assert_eq!(end.outcome, TurnOutcome::Completed);
        assert_eq!(world.provider.remaining(), 0, "the run went on");
        assert_eq!(world.read(PROTECTED), None);
        assert_eq!(front.results()[0].1, "denied");
        assert_eq!(front.last_text(), "I could not write it.");

        let denials = core.permission_denials(&front.session);
        assert_eq!(denials.len(), 1);
        assert_eq!(denials[0].tool, "write");
        assert_eq!(denials[0].call_id.as_str(), "c1");
        assert_eq!(denials[0].turn_id.as_str(), "t_1");
        assert!(!denials[0].reason.is_empty());
        front.close().await;
    });
}

#[test]
fn with_fail_on_denial_a_call_nobody_can_answer_ends_the_turn_as_denied() {
    let world = World::new(
        "fail_on_denial",
        [protected_write("c1"), says("never sent")],
    );
    let core = world.core_with(|options| options.fail_on_denial = true);
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        let end = front.turn("set up CI").await;
        assert_eq!(end.outcome, TurnOutcome::Denied);
        let error = end.error.expect("the turn says why it ended");
        assert_eq!(error.code, "permission_denied");
        assert_eq!(error.kind, cortex_protocol::ErrorKind::Permission);
        // The turn stopped there: the model was not asked again.
        assert_eq!(
            world.provider.remaining(),
            1,
            "the run went on after the denial"
        );
        assert_eq!(world.read(PROTECTED), None);
        assert_eq!(core.permission_denials(&front.session).len(), 1);
        front.close().await;
    });
}

#[test]
fn fail_on_denial_changes_nothing_when_somebody_can_answer() {
    let world = World::new("fail_asked", [protected_write("c1"), says("Done.")]);
    let core = world.core_with(|options| options.fail_on_denial = true);
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("set up CI").await;
        let (id, _) = front.approval().await;
        front.answer(&id, ApprovalChoice::Deny).unwrap();
        front.turns(1).await;
        // A person refused: a result for the model, not the end of the turn.
        assert_eq!(front.ends()[0].outcome, TurnOutcome::Completed);
        assert!(core.permission_denials(&front.session).is_empty());
        front.close().await;
    });
}

#[test]
fn an_approval_nobody_answers_in_time_is_a_denial() {
    let world = World::new("timeout", [protected_write("c1"), says("Nobody answered.")]);
    let core = world.core_with(|options| options.approval_timeout = Some(Duration::from_secs(30)));
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("set up CI").await;
        let (id, _) = front.approval().await;
        settle().await;
        assert!(world.timer.running().contains(&Duration::from_secs(30)));
        world.timer.advance(Duration::from_secs(29));
        settle().await;
        assert!(front.ends().is_empty(), "the limit has not passed");
        world.timer.advance(Duration::from_secs(2));
        front.turns(1).await;
        assert_eq!(front.results()[0].1, "denied");
        assert_eq!(world.read(PROTECTED), None);
        assert_eq!(resolutions(&front), [Resolution::TimedOut]);
        // The answer that comes after it is too late.
        let late = front.answer(&id, ApprovalChoice::AllowOnce).unwrap_err();
        assert_eq!(late.code, "request_not_open");
        front.close().await;
    });
}

#[test]
fn an_interrupt_withdraws_an_open_question() {
    let world = World::new("withdrawn", [protected_write("c1")]);
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("set up CI").await;
        let (id, _) = front.approval().await;
        front.interrupt(false).await;
        front.turns(1).await;
        assert_eq!(front.ends()[0].outcome, TurnOutcome::Interrupted);
        assert_eq!(resolutions(&front), [Resolution::Cancelled]);
        assert_eq!(
            front
                .answer(&id, ApprovalChoice::AllowOnce)
                .unwrap_err()
                .code,
            "request_not_open"
        );
        assert_eq!(world.read(PROTECTED), None);
        front.close().await;
    });
}

#[test]
fn allowed_for_the_session_is_not_asked_again_and_is_gone_with_the_session() {
    let world = World::new(
        "allow_session",
        [
            protected_write("c1"),
            calls(&[("c2", "read", json!({"path": PROTECTED}))]),
            calls(&[(
                "c3",
                "write",
                json!({"path": PROTECTED, "content": "on: pull_request\n"}),
            )]),
            says("Done."),
            protected_write("c4"),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("set up CI").await;
        let (id, request) = front.approval().await;
        assert!(request.choices.contains(&ApprovalChoice::AllowSession));
        // What the answer would add is shown before it is given.
        assert_eq!(request.adds.len(), 1, "{:?}", request.adds);
        assert_eq!(request.adds[0].effect, Effect::Write);
        front.answer(&id, ApprovalChoice::AllowSession).unwrap();
        // The second write to the same file is not asked about.
        front.turns(1).await;
        assert_eq!(world.read(PROTECTED).as_deref(), Some("on: pull_request\n"));
        let listed = front
            .op(Op::PermissionsList(SessionRef {
                session_id: front.session.clone(),
            }))
            .await
            .unwrap();
        let OpResult::PermissionsList(listed) = listed else {
            panic!("not approvals");
        };
        assert_eq!(listed.approvals.len(), 1);
        front.close().await;

        // A new session asks again.
        let front = Front::start(&world, &core).await;
        front.prompt("again").await;
        let (_, request) = front.approval().await;
        assert_eq!(request.call_id.as_str(), "c4");
        front.interrupt(false).await;
        front.turns(1).await;
        front.close().await;
    });
}

#[test]
fn allowed_for_the_project_is_stored_outlives_the_run_and_can_be_revoked() {
    let world = World::new(
        "allow_project",
        [
            protected_write("c1"),
            says("Done."),
            calls(&[("c2", "read", json!({"path": PROTECTED}))]),
            protected_write("c3"),
            says("Again."),
            protected_write("c4"),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("set up CI").await;
        let (id, request) = front.approval().await;
        assert!(request.choices.contains(&ApprovalChoice::AllowProject));
        front.answer(&id, ApprovalChoice::AllowProject).unwrap();
        front.turns(1).await;
        front.close().await;
    });
    // In the user's state directory, never in the repository.
    let state = world.dirs().state.unwrap();
    let stored: Vec<_> = std::fs::read_dir(state.join("approvals"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(stored.len(), 1);
    assert!(!world.workspace.join(".cortex").exists());

    // Another run of the program: the approval still stands.
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let end = front.turn("once more").await;
        assert_eq!(end.outcome, TurnOutcome::Completed);
        assert!(
            !front
                .seen
                .borrow()
                .iter()
                .any(|outgoing| matches!(outgoing, Outgoing::Request { .. })),
            "a stored approval was asked about again"
        );

        // Revoked, it asks again.
        let listed = front
            .op(Op::PermissionsList(SessionRef {
                session_id: front.session.clone(),
            }))
            .await
            .unwrap();
        let OpResult::PermissionsList(listed) = listed else {
            panic!("not approvals");
        };
        assert_eq!(listed.approvals.len(), 1);
        front
            .op(Op::PermissionsRevoke(PermissionsRevokeParams {
                session_id: front.session.clone(),
                ids: vec![listed.approvals[0].id.clone()],
            }))
            .await
            .unwrap();
        front.prompt("and again").await;
        let (_, request) = front.approval().await;
        assert_eq!(request.call_id.as_str(), "c4");
        front.interrupt(false).await;
        front.turns(2).await;
        front.close().await;
    });
}
