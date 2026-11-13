//! The sandbox as the core wires it (`docs/architecture/06-permissions-and-sandbox.md`,
//! "The sandbox"; `docs/decisions/0032-what-the-linux-sandbox-needs.md`):
//! the state a session reports, the levels it lets start, and on Linux
//! commands confined end to end. The tests that start a shell are named
//! `sys_…`.

use std::ffi::OsString;
use std::path::PathBuf;

use cortex_core::Outgoing;
use cortex_protocol::SessionEvent;
use cortex_protocol::event::{Event, Notice};
use cortex_protocol::lease::Level;
use cortex_protocol::methods::{InitializeParams, Op, OpResult};
use cortex_protocol::session::{SandboxStatus, SessionSettings};
use cortex_sandbox::support::Support;
use serde_json::json;

use crate::{Front, World, calls, says};

/// This test binary as the sandbox launcher: it enters [`crate::launcher_entry`].
fn launcher() -> (PathBuf, Vec<OsString>) {
    (
        std::env::current_exe().expect("the test binary has a path"),
        ["--exact", "launcher_entry", "--nocapture", crate::LAUNCH]
            .map(OsString::from)
            .to_vec(),
    )
}

/// A session at `level`.
fn at(level: Level) -> SessionSettings {
    SessionSettings {
        level: Some(level),
        ..SessionSettings::default()
    }
}

/// What the `initialize` result says of the sandbox.
async fn initialized(front: &Front) -> SandboxStatus {
    let result = front
        .core
        .handle(
            front.client.id,
            Op::Initialize(
                serde_json::from_value::<InitializeParams>(json!({
                    "protocol_version": cortex_protocol::methods::PROTOCOL_VERSION,
                    "client": {"name": "test", "version": "0"},
                    "role": "user",
                }))
                .expect("initialize parameters"),
            ),
        )
        .await
        .expect("initialize");
    let OpResult::Initialize(result) = result else {
        panic!("not the result of initialize");
    };
    result.capabilities.sandbox
}

/// Waits for the session's notices until one says what `wanted` looks for,
/// and returns them all. The notices are sent when the session opens.
async fn notices_until(front: &Front, wanted: impl Fn(&str) -> bool) -> Vec<String> {
    fn notices_in(seen: &[Outgoing]) -> Vec<String> {
        seen.iter()
            .filter_map(|outgoing| match outgoing {
                Outgoing::Event(SessionEvent {
                    event: Event::Notice(Notice::Info(info)),
                    ..
                }) => Some(info.message.clone()),
                Outgoing::Event(SessionEvent {
                    event: Event::Notice(Notice::Sandbox(sandbox)),
                    ..
                }) => Some(sandbox.message.clone()),
                _ => None,
            })
            .collect()
    }
    front
        .until(|seen| notices_in(seen).iter().any(|notice| wanted(notice)))
        .await;
    notices_in(&front.seen.borrow())
}

#[test]
fn a_kernel_without_landlock_is_reported_and_refuses_autonomous() {
    let world = World::new("no_landlock", []);
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.sandbox = Some(Support::NoLandlock);
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.sandbox, SandboxStatus::Off);
        assert_eq!(initialized(&front).await, SandboxStatus::Off);
        let notices = notices_until(&front, |notice| {
            notice.starts_with("sandbox: off.") && notice.contains("no Landlock")
        })
        .await;
        assert!(!notices.is_empty());
        let refused = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .err()
        .expect("autonomous is refused");
        assert_eq!(refused.code, "sandbox_required");
        assert!(
            refused.message.contains("no Landlock"),
            "{}",
            refused.message
        );
    });
}

#[test]
fn an_old_landlock_does_not_count_and_says_what_it_lacks() {
    let world = World::new("old_landlock", []);
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.sandbox = Some(Support::Landlock(2));
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.sandbox, SandboxStatus::Off);
        notices_until(&front, |notice| {
            notice.contains("version 2") && notice.contains("empty any file")
        })
        .await;
        let refused = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .err()
        .expect("autonomous is refused");
        assert_eq!(refused.code, "sandbox_required");
    });
}

#[test]
fn the_sandbox_turned_off_or_without_a_launcher_is_off() {
    let world = World::new("turned_off", []);
    world.user_config("[sandbox]\nenabled = false\n");
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.sandbox = Some(Support::Landlock(8));
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.sandbox, SandboxStatus::Off);
        notices_until(&front, |notice| notice.contains("sandbox.enabled = false")).await;
    });
    // A core given no launcher has nothing to confine with.
    let world = World::new("no_launcher", []);
    let core = world.core_with(|options| options.sandbox = Some(Support::Landlock(8)));
    world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.sandbox, SandboxStatus::Off);
        assert_eq!(initialized(&front).await, SandboxStatus::Off);
    });
}

#[cfg(target_os = "linux")]
#[test]
fn a_landlock_with_a_gap_enforces_and_names_the_gap() {
    let world = World::new("landlock_gap", []);
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.sandbox = Some(Support::Landlock(4));
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.sandbox, SandboxStatus::Enforcing);
        assert_eq!(initialized(&front).await, SandboxStatus::Enforcing);
        let notices = notices_until(&front, |notice| {
            notice.starts_with("sandbox:") && notice.contains("signal")
        })
        .await;
        assert!(
            !notices
                .iter()
                .any(|notice| notice.starts_with("sandbox: off."))
        );
        Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("autonomous starts where the sandbox enforces");
    });
}

/// Whether the sandbox enforces on this machine. On CI's Linux runners it
/// must: there a kernel without it fails the test rather than skipping it.
#[cfg(target_os = "linux")]
fn enforcing_here() -> bool {
    let enforcing = matches!(
        cortex_sandbox::support::detect().assess(),
        cortex_sandbox::support::Assessment::Enforcing { .. }
    );
    assert!(
        enforcing || std::env::var_os("CI").is_none(),
        "CI must run the sandbox's tests on a kernel where it enforces"
    );
    enforcing
}

/// A command that writes inside the workspace, into its temporary
/// directory and outside both, and says where its temporary directory is.
#[cfg(target_os = "linux")]
fn writes(outside: &std::path::Path) -> String {
    format!(
        "echo in > inside.txt; printf %s \"$TMPDIR\" > tmpdir.txt; echo t > \"$TMPDIR/t\" && \
         cat \"$TMPDIR/t\" > from-tmp.txt; echo x > '{}/f'",
        outside.display()
    )
}

#[cfg(target_os = "linux")]
#[test]
fn sys_commands_are_confined_where_the_sandbox_enforces() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("confined", []);
    let outside = world.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    world.provider.push(calls(&[(
        "c1",
        "shell",
        json!({"command": writes(&outside)}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| options.launcher = Some(launcher()));
    let temp = world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.sandbox, SandboxStatus::Enforcing);
        assert_eq!(initialized(&front).await, SandboxStatus::Enforcing);
        front.turn_allowing("write three files").await;

        // The approval said the command would run inside the sandbox.
        let approvals = front.approvals();
        assert_eq!(approvals.len(), 1, "{approvals:?}");
        let cortex_protocol::methods::ApprovalDetail::Command(detail) = &approvals[0].detail else {
            panic!("not a command's approval: {:?}", approvals[0].detail);
        };
        assert!(!detail.outside_sandbox);

        // Inside the workspace and the temporary directory: written.
        assert_eq!(world.read("inside.txt").as_deref(), Some("in\n"));
        assert_eq!(world.read("from-tmp.txt").as_deref(), Some("t\n"));
        let temp = PathBuf::from(world.read("tmpdir.txt").expect("the command said"));
        assert!(
            temp.ends_with(format!("cortex-harness-{}", front.session)),
            "{}",
            temp.display()
        );
        assert!(temp.is_dir());
        assert!(cortex_base::private::is_private(&temp).unwrap());
        // Outside: refused, and the model is told which write.
        assert!(!outside.join("f").exists());
        let results = front.results();
        assert!(
            results[0].2.contains("The sandbox refused a write to")
                && results[0].2.contains(&outside.display().to_string()),
            "{results:?}"
        );

        // `autonomous` can start.
        Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("autonomous starts where the sandbox enforces");
        core.shutdown().await;
        temp
    });
    // The temporary directory goes with the session.
    assert!(!temp.exists(), "{}", temp.display());
}

#[cfg(target_os = "linux")]
#[test]
fn sys_at_unrestricted_a_command_is_not_confined() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("unrestricted", []);
    let outside = world.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    world.provider.push(calls(&[(
        "c1",
        "shell",
        json!({"command": writes(&outside)}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| options.launcher = Some(launcher()));
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Unrestricted),
        )
        .await
        .expect("the session starts");
        // The machine's sandbox enforces; this session's commands are not
        // put in it, and its state says so.
        assert_eq!(front.state.sandbox, SandboxStatus::Off);
        front.turn_allowing("write three files").await;
        assert_eq!(
            std::fs::read_to_string(outside.join("f")).ok().as_deref(),
            Some("x\n")
        );
    });
}
