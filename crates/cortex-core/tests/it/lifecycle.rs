//! When a session ends: "Sessions end when their last client closes them
//! or disconnects" (`docs/architecture/11-rpc-protocol.md`), and every
//! session ends at shutdown.
//!
//! A session is put together off the core's thread, so a client can leave,
//! and a shutdown begin, while one is being put together for it. These
//! tests make that happen at a known step: every piece of blocking work
//! waits one turn of the runtime ([`World::one_turn_each`]), and the test
//! polls the operation by hand and acts between two of its steps. Found by
//! the review of step 13a.

use cortex_core::ClientInfo;
use cortex_protocol::error::code;
use cortex_protocol::methods::{Op, SessionForkParams, SessionNewParams};
use cortex_protocol::session::SessionSettings;

use crate::{Front, World, poll_once, says};

fn new_session(world: &World, ephemeral: bool) -> Op {
    Op::SessionNew(SessionNewParams {
        workspace: world.workspace.to_str().unwrap().to_owned(),
        settings: SessionSettings::default(),
        title: None,
        config: serde_json::Map::new(),
        ephemeral,
    })
}

/// A client that disconnects while its `session/new` is being put
/// together leaves no session running with nobody on it.
#[test]
fn a_session_started_for_a_client_that_left_meanwhile_does_not_run_on() {
    let mut world = World::new("left_during_new", [says("Hi.")]);
    world.one_turn_each();
    let core = world.core();
    world.run(async {
        let leaving = core.connect(ClientInfo::user());
        let mut started = std::pin::pin!(core.handle(leaving.id, new_session(&world, true)));
        // The session is being put together: that work waits one turn.
        assert!(poll_once(started.as_mut()).await.is_pending());

        core.disconnect(leaving.id).await;
        let refused = started.await.expect_err("a session for a client that left");
        assert_eq!(refused.code, code::NOT_INITIALIZED);
        assert_eq!(core.open_sessions(), Vec::new(), "a session runs on");

        // A client that stays is given its session as before.
        let staying = Front::start(&world, &core).await;
        assert_eq!(core.open_sessions(), vec![staying.session.clone()]);
        staying.close().await;
        assert_eq!(core.open_sessions(), Vec::new());
    });
}

/// The same for `session/fork`, at each step: a client that leaves while
/// the fork is written refuses before any session is put together, and
/// one that leaves while the fork is put together leaves nothing running.
/// The session forked from was the client's alone, so it ends too.
#[test]
fn a_fork_for_a_client_that_left_meanwhile_does_not_run_on() {
    let mut world = World::new("left_during_fork", [says("One."), says("Two.")]);
    world.one_turn_each();
    let core = world.core();
    world.run(async {
        // Leaves after the first step (the fork written to disk), then
        // after the second (the fork put together).
        for steps in [1, 2] {
            let front = Front::start(&world, &core).await;
            front.turn("hello").await;
            let mut forked = std::pin::pin!(front.op(Op::SessionFork(SessionForkParams {
                session_id: front.session.clone(),
                at: None,
            })));
            for _ in 0..steps {
                assert!(
                    poll_once(forked.as_mut()).await.is_pending(),
                    "the fork was over in fewer than {steps} steps"
                );
            }
            core.disconnect(front.client.id).await;
            let refused = forked.await.expect_err("a fork for a client that left");
            assert_eq!(refused.code, code::NOT_INITIALIZED, "after {steps} steps");
            assert_eq!(
                core.open_sessions(),
                Vec::new(),
                "after {steps} steps, a session runs on"
            );
        }
    });
}

/// A shutdown that begins while a session is being put together ends that
/// session too, and no session starts after it.
#[test]
fn a_session_started_while_the_core_shuts_down_does_not_run_on() {
    let mut world = World::new("shutdown_during_new", [says("Hi.")]);
    world.one_turn_each();
    let core = world.core();
    world.run(async {
        let client = core.connect(ClientInfo::user());
        let mut started = std::pin::pin!(core.handle(client.id, new_session(&world, false)));
        assert!(poll_once(started.as_mut()).await.is_pending());

        core.shutdown().await;
        let refused = started
            .await
            .expect_err("a session started under a shutdown");
        assert_eq!(refused.code, code::SHUTTING_DOWN);
        assert_eq!(core.open_sessions(), Vec::new(), "a session runs on");

        // And after it, none starts.
        let after = core
            .handle(client.id, new_session(&world, true))
            .await
            .expect_err("a session started after a shutdown");
        assert_eq!(after.code, code::SHUTTING_DOWN);
        assert_eq!(core.open_sessions(), Vec::new());
    });
}

/// A session another client still has open is not ended when the client
/// that started it leaves, and goes on for the one that stayed.
#[test]
fn a_session_another_client_joined_goes_on_when_its_first_client_leaves() {
    let world = World::new("first_leaves", [says("Hi."), says("Still here.")]);
    let core = world.core();
    world.run(async {
        let first = Front::start(&world, &core).await;
        let second = first.join(ClientInfo::user()).await;
        core.disconnect(first.client.id).await;
        assert_eq!(core.open_sessions(), vec![first.session.clone()]);
        second.turn("hello").await;
        assert_eq!(second.last_text(), "Hi.");
        core.disconnect(second.client.id).await;
        assert_eq!(core.open_sessions(), Vec::new());
    });
}
