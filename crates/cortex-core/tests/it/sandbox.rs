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

use crate::{Front, World};
// What only the tests that start a shell use, which run on Linux.
#[cfg(target_os = "linux")]
use crate::{calls, says};

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

#[cfg(target_os = "linux")]
#[test]
fn sys_a_command_leaves_the_sandbox_only_once_the_user_allows_it() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("leave", []);
    let outside = world.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // `tee` names the file as an argument: the analysis lets it run at
    // `autonomous`, and the sandbox is what refuses it.
    let write = format!("echo x | tee '{}/f'", outside.display());
    // At `autonomous`: the same command runs unasked in the sandbox and is
    // refused there, then is asked about outside it and runs.
    world
        .provider
        .push(calls(&[("c1", "shell", json!({"command": write}))]));
    world.provider.push(calls(&[(
        "c2",
        "shell",
        json!({"command": write, "sandbox": false}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| options.launcher = Some(launcher()));
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("the session starts");
        let asked = front.turn_allowing("write outside").await;
        assert_eq!(
            asked,
            vec!["shell".to_owned()],
            "only the call that leaves is asked"
        );
        let approvals = front.approvals();
        let cortex_protocol::methods::ApprovalDetail::Command(detail) = &approvals[0].detail else {
            panic!("not a command's approval: {:?}", approvals[0].detail);
        };
        assert!(detail.outside_sandbox);
        assert_eq!(
            approvals[0].choices,
            vec![
                cortex_protocol::methods::ApprovalChoice::AllowOnce,
                cortex_protocol::methods::ApprovalChoice::Deny
            ]
        );
        let results = front.results();
        assert!(
            results[0].2.contains("\"sandbox\": false"),
            "the model is told the way out: {results:?}"
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("f")).ok().as_deref(),
            Some("x\n")
        );
    });
}

/// The first assistant message's id: where a rewind goes back to.
#[cfg(target_os = "linux")]
fn first_message(front: &Front) -> cortex_protocol::ItemId {
    front
        .items()
        .iter()
        .find_map(|item| match item {
            cortex_protocol::Item::AssistantMessage(message) => Some(message.id.clone()),
            _ => None,
        })
        .expect("an assistant message")
}

#[cfg(target_os = "linux")]
#[test]
fn sys_a_protected_path_a_command_changes_stops_the_turn_and_undo_puts_it_back() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("protected", []);
    world.put(".git/config", "[core]\n");
    world.put(".git/hooks/pre-commit.sample", "#!/bin/sh\n");
    // A script that writes git's hook and configuration without its
    // command line naming either: the analysis cannot see it.
    world.put(
        "fix.sh",
        "echo evil > .git/hooks/pre-commit\nprintf '[core]\\n\\tfsmonitor = ./run\\n' > .git/config\n",
    );
    world.provider.push(says("Ready."));
    world
        .provider
        .push(calls(&[("c1", "shell", json!({"command": "sh fix.sh"}))]));
    world.provider.push(calls(&[(
        "c2",
        "shell",
        json!({"command": "echo after > after.txt"}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| options.launcher = Some(launcher()));
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("the session starts");
        front.turn("get ready").await;
        let keep = first_message(&front);
        let ended = front.turn("fix it").await;
        assert!(
            front.approvals().is_empty(),
            "nothing was asked at autonomous"
        );

        // Found and shown.
        let results = front.results();
        let shown = &results[0].2;
        assert!(
            shown.contains("changed protected paths")
                && shown.contains("`.git/hooks/pre-commit` (created)")
                && shown.contains("`.git/config` (modified)")
                && shown.contains("Undoing this turn puts them back"),
            "{results:?}"
        );
        // The turn stopped at its next call, which did not run.
        assert_eq!(
            ended.outcome,
            cortex_protocol::event::TurnOutcome::Denied,
            "{ended:?}"
        );
        assert!(world.read("after.txt").is_none());
        assert_eq!(
            world.read(".git/hooks/pre-commit").as_deref(),
            Some("evil\n")
        );

        // Undoing the turn puts both back.
        front
            .op(cortex_protocol::methods::Op::SessionRewind(
                cortex_protocol::methods::SessionRewindParams {
                    session_id: front.session.clone(),
                    to: keep,
                    restore_files: true,
                },
            ))
            .await
            .unwrap();
        assert_eq!(world.read(".git/config").as_deref(), Some("[core]\n"));
        assert!(world.read(".git/hooks/pre-commit").is_none());
        assert!(world.read(".git/hooks/pre-commit.sample").is_some());
        front.close().await;
    });
}

#[cfg(target_os = "linux")]
#[test]
fn sys_past_its_bound_the_look_at_protected_paths_warns_and_does_not_stop() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("protected_bound", []);
    world.put(".git/config", "[core]\n");
    world.put("a/b/c/file.txt", "x\n");
    world.provider.push(calls(&[(
        "c1",
        "shell",
        json!({"command": "echo one > one.txt"}),
    )]));
    world.provider.push(calls(&[(
        "c2",
        "shell",
        json!({"command": "echo two > two.txt"}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.protect_limits = Some(cortex_sandbox::protect::Limits {
            directories: 1,
            ..cortex_sandbox::protect::Limits::default()
        });
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("the session starts");
        let ended = front.turn("write two files").await;
        let results = front.results();
        assert!(
            results[0].2.contains("looked at only in part"),
            "{results:?}"
        );
        // A warning, not a stop: the next call ran, and the turn ended as
        // the model ended it.
        assert_eq!(world.read("two.txt").as_deref(), Some("two\n"));
        assert_eq!(
            ended.outcome,
            cortex_protocol::event::TurnOutcome::Completed,
            "{ended:?}"
        );
    });
}

#[test]
fn a_workspace_that_holds_the_harness_files_is_not_confined() {
    let world = World::new("own_files", []);
    // A write root that holds the home directory holds the harness's
    // configuration, state and data too.
    world.user_config(&format!(
        "[sandbox]\nwrite = [\"{}\"]\n",
        world.home.display().to_string().replace('\\', "\\\\")
    ));
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.sandbox = Some(Support::Landlock(8));
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.sandbox, SandboxStatus::Off);
        notices_until(&front, |notice| notice.contains("own configuration")).await;
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

#[cfg(target_os = "linux")]
#[test]
fn sys_with_no_landlock_a_command_runs_unconfined() {
    let world = World::new("unconfined_no_landlock", []);
    let outside = world.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    world.provider.push(calls(&[(
        "c1",
        "shell",
        json!({"command": writes(&outside)}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.sandbox = Some(Support::NoLandlock);
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn_allowing("write three files").await;
        assert_eq!(
            std::fs::read_to_string(outside.join("f")).ok().as_deref(),
            Some("x\n"),
            "{:?}",
            front.results()
        );
    });
}

#[cfg(target_os = "linux")]
#[test]
fn sys_a_change_of_level_takes_the_sandbox_with_it() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("level_change", []);
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
        assert_eq!(front.state.sandbox, SandboxStatus::Off);
        let result = front
            .op(Op::SessionSet(cortex_protocol::methods::SessionSetParams {
                session_id: front.session.clone(),
                settings: at(Level::Autonomous),
            }))
            .await
            .expect("the level is raised");
        let OpResult::SessionSet(result) = result else {
            panic!("not the result of session/set");
        };
        assert_eq!(result.session.sandbox, SandboxStatus::Enforcing);
        front.turn_allowing("write three files").await;
        // Confined now: the write outside is refused.
        assert!(!outside.join("f").exists(), "{:?}", front.results());
    });
}

#[cfg(target_os = "linux")]
#[test]
fn sys_a_stopped_turn_runs_no_more_commands_of_the_same_response() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("stopped_in_one_response", []);
    world.put(".git/config", "[core]\n");
    world.put(".git/hooks/pre-commit.sample", "#!/bin/sh\n");
    world.put("fix.sh", "echo evil > .git/hooks/pre-commit\n");
    // Both calls in one response: the second is decided before the first
    // runs.
    world.provider.push(calls(&[
        ("c1", "shell", json!({"command": "sh fix.sh"})),
        ("c2", "shell", json!({"command": "echo after > after.txt"})),
    ]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| options.launcher = Some(launcher()));
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("the session starts");
        front.turn("fix it").await;
        assert!(world.read(".git/hooks/pre-commit").is_some());
        assert!(world.read("after.txt").is_none(), "{:?}", front.results());
    });
}

#[cfg(target_os = "linux")]
#[test]
fn sys_undo_does_not_write_through_a_link_a_command_planted() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("undo_through_link", []);
    world.put(".git/config", "[core]\n");
    world.put(".git/hooks/pre-commit.sample", "#!/bin/sh\n");
    let elsewhere = world.dir.join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("hooks")).unwrap();
    std::fs::write(elsewhere.join("config"), "theirs\n").unwrap();
    // The command swaps the repository's directory for a link to a
    // directory outside the workspace.
    world.put(
        "swap.sh",
        format!("mv .git .git.old && ln -s '{}' .git\n", elsewhere.display()),
    );
    world.provider.push(says("Ready."));
    world
        .provider
        .push(calls(&[("c1", "shell", json!({"command": "sh swap.sh"}))]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| options.launcher = Some(launcher()));
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("the session starts");
        front.turn("get ready").await;
        let keep = first_message(&front);
        front.turn("swap it").await;
        let results = front.results();
        assert!(results[0].2.contains("`.git` (created)"), "{results:?}");
        let _ = front
            .op(Op::SessionRewind(
                cortex_protocol::methods::SessionRewindParams {
                    session_id: front.session.clone(),
                    to: keep,
                    restore_files: true,
                },
            ))
            .await;
        // Nothing outside the workspace was written by the undo.
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("config")).unwrap(),
            "theirs\n"
        );
        assert!(!elsewhere.join("hooks/pre-commit.sample").exists());
        front.close().await;
    });
}
