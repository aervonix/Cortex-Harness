//! The shell tool as the core builds it: what a command is given, what
//! becomes of what it changed, and the second interrupt
//! (`docs/architecture/05-tools.md`, "`shell`";
//! `docs/architecture/06-permissions-and-sandbox.md`, "Command analysis as
//! built"). These start a shell, so they are named `sys_…`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use cortex_protocol::event::{Delta, Event, TurnOutcome};
use cortex_protocol::methods::{Op, SessionRewindParams};
use cortex_protocol::{Item, Part};
use serde_json::json;

use crate::{Front, World, calls, says};

/// A provider key, made up.
const KEY: &str = "cortex-shell-test-key-77e1";

/// The command that prints the environment, one `NAME=value` a line.
fn print_env() -> &'static str {
    if cfg!(windows) { "cmd /c set" } else { "env" }
}

fn shell_text(front: &Front) -> String {
    front
        .items()
        .into_iter()
        .find_map(|item| match item {
            Item::ToolResult(result) if result.tool == "shell" => Some(
                result
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .expect("a shell result")
}

#[test]
fn sys_a_command_is_given_the_environment_the_command_analysis_relies_on() {
    let world = World::new(
        "shell_env",
        [
            calls(&[("c1", "shell", json!({"command": print_env()}))]),
            says("Seen."),
        ],
    );
    world.user_config(
        "[providers.mine]\nprotocol = \"anthropic-messages\"\nbase_url = \
         \"https://llm.example.invalid\"\nkey_env = \"MY_PROVIDER_KEY\"\n\n[shell]\nenv_remove = \
         [\"EXTRA_REMOVED\", \"PREFIXED_*\"]\n",
    );
    let separator = if cfg!(windows) { ';' } else { ':' };
    let real_path = std::env::var("PATH").unwrap();
    // The user's own `PATH`, with the two kinds of entry that name the
    // directory a command runs in.
    let path = format!(".{separator}{real_path}{separator}{separator}bin");
    let core = world.core_with(|options| {
        let mut env = world.shell_env(&[
            ("CDPATH", "/somewhere/else"),
            ("RIPGREP_CONFIG_PATH", "/etc/ripgreprc"),
            // The user already gives git a setting through its environment.
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "user.name"),
            ("GIT_CONFIG_VALUE_0", "Dev"),
            ("ANTHROPIC_API_KEY", KEY),
            ("MY_PROVIDER_KEY", KEY),
            ("EXTRA_REMOVED", "gone"),
            ("PREFIXED_ONE", "gone"),
            ("KEPT", "yes"),
        ]);
        env.retain(|(name, _)| !name.eq_ignore_ascii_case("PATH"));
        env.push(("PATH".into(), path.clone().into()));
        options.shell_env = Some(env);
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn_allowing("show the environment").await;
        let text = shell_text(&front);
        let env: BTreeMap<String, String> = text
            .lines()
            .filter_map(|line| line.trim_end().split_once('='))
            .map(|(name, value)| (name.to_ascii_uppercase(), value.to_owned()))
            .collect();
        assert_eq!(env.get("KEPT").map(String::as_str), Some("yes"), "{text}");

        // What the analysis takes to be unset is unset.
        assert_eq!(env.get("CDPATH"), None, "{text}");
        assert_eq!(env.get("RIPGREP_CONFIG_PATH"), None, "{text}");

        // Git's monitor is off, after the setting the user already gives.
        assert_eq!(env.get("GIT_CONFIG_COUNT").map(String::as_str), Some("2"));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_0").map(String::as_str),
            Some("user.name")
        );
        assert_eq!(
            env.get("GIT_CONFIG_VALUE_0").map(String::as_str),
            Some("Dev")
        );
        assert_eq!(
            env.get("GIT_CONFIG_KEY_1").map(String::as_str),
            Some("core.fsmonitor")
        );
        assert_eq!(
            env.get("GIT_CONFIG_VALUE_1").map(String::as_str),
            Some("false")
        );

        // No provider's key: the built-in one's, a configured one's, and
        // what configuration lists.
        assert_eq!(env.get("ANTHROPIC_API_KEY"), None);
        assert_eq!(env.get("MY_PROVIDER_KEY"), None);
        assert_eq!(env.get("EXTRA_REMOVED"), None);
        assert_eq!(env.get("PREFIXED_ONE"), None);
        assert!(!text.contains(KEY), "{text}");

        // `PATH` names only absolute directories.
        let given = env.get("PATH").expect("a PATH");
        for entry in given.split(separator) {
            assert!(
                Path::new(entry.trim_matches('"')).is_absolute(),
                "`{entry}` in the PATH a command was given: {given}"
            );
        }
        assert!(given.split(separator).count() >= 1);
        front.close().await;
    });
}

/// A command that ignores the polite interrupt sits out a grace that
/// nothing ends in this test. The second interrupt kills it.
#[cfg(unix)]
#[test]
fn sys_a_second_interrupt_kills_a_command_that_ignores_the_first() {
    let world = World::new(
        "kill_now",
        [calls(&[(
            "c1",
            "shell",
            json!({"command": "trap '' INT TERM; echo started; sleep 600"}),
        )])],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let before = front.ends().len();
        front.prompt("run it").await;
        // Allow the command, and wait for it to say it is running.
        let (id, _) = front.approval().await;
        front
            .answer(&id, cortex_protocol::methods::ApprovalChoice::AllowOnce)
            .unwrap();
        front
            .until(|seen| {
                seen.iter().any(|outgoing| {
                    matches!(outgoing, cortex_core::Outgoing::Event(event)
                    if matches!(&event.event, Event::ItemDelta(delta)
                        if matches!(&delta.delta, Delta::ToolOutput(text) if text.text.contains("started"))))
                })
            })
            .await;

        front.interrupt(false).await;
        // The polite stage was sent, and its grace is running. Nobody ends
        // it, and the command does not listen.
        for _ in 0..10_000 {
            if world
                .timer
                .running()
                .contains(&cortex_tools::process::DEFAULT_GRACE)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            world
                .timer
                .running()
                .contains(&cortex_tools::process::DEFAULT_GRACE),
            "the command was not asked to stop"
        );
        front.drain();
        assert_eq!(front.ends().len(), before, "the first interrupt ended the turn");

        front.interrupt(true).await;
        front.turns(before + 1).await;
        assert_eq!(front.ends()[before].outcome, TurnOutcome::Interrupted);
        let payload = front
            .items()
            .into_iter()
            .find_map(|item| match item {
                Item::ToolResult(result) if result.tool == "shell" => result.payload,
                _ => None,
            })
            .expect("the command has a result");
        assert_eq!(payload["stopped_with"], "kill");
        assert_eq!(payload["interrupted"], true);
        front.close().await;
    });
}

fn git(dir: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .current_dir(dir)
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn sys_undo_puts_back_what_a_command_changed_where_git_still_has_it() {
    let world = World::new(
        "shell_undo",
        [
            says("Ready."),
            calls(&[(
                "c1",
                "shell",
                json!({"command": "echo changed > tracked.txt; echo new > created.txt"}),
            )]),
            says("Changed."),
        ],
    );
    world.put("tracked.txt", "committed\n");
    world.put("dirty.txt", "committed\n");
    if !(git(&world.workspace, &["init", "-q"])
        && git(&world.workspace, &["add", "."])
        && git(&world.workspace, &["commit", "-q", "-m", "first"]))
    {
        return;
    }
    let committed = std::fs::read(world.workspace.join("tracked.txt")).unwrap();
    let core = world.core_with(|options| {
        options.trust_project = true;
        // The shell is looked for and asked its version, as in the real
        // program, and the tool's description names it.
        options.shell = None;
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("hello").await;
        let description = world.provider.requests()[0]
            .tools
            .iter()
            .find(|tool| tool.name == "shell")
            .map(|tool| tool.description.clone())
            .expect("a shell tool");
        assert!(
            description
                .chars()
                .any(|character| character.is_ascii_digit()),
            "the shell's version is not in the tool's description: {description}"
        );
        let keep = front
            .items()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::AssistantMessage(message) => Some(message.id.clone()),
                _ => None,
            })
            .unwrap();
        front.turn_allowing("change things").await;
        assert_ne!(
            std::fs::read(world.workspace.join("tracked.txt")).unwrap(),
            committed
        );
        assert!(world.workspace.join("created.txt").exists());

        // The round's checkpoint: the tracked file with the content git
        // has, and the new file as one that did not exist.
        let log = front.log(&world);
        let checkpoint = log
            .iter()
            .find(|line| line["kind"] == "checkpoint")
            .expect("the command's round has a checkpoint");
        let files = checkpoint["files"].as_array().unwrap();
        let file = |path: &str| files.iter().find(|file| file["path"] == path).unwrap();
        assert!(file("tracked.txt")["before"].is_object(), "{checkpoint}");
        assert!(file("created.txt")["before"].is_null(), "{checkpoint}");

        front
            .op(Op::SessionRewind(SessionRewindParams {
                session_id: front.session.clone(),
                to: keep,
                restore_files: true,
            }))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(world.workspace.join("tracked.txt")).unwrap(),
            committed,
            "the file git still had was not put back"
        );
        assert!(
            !world.workspace.join("created.txt").exists(),
            "the file the command created is still there"
        );
        front.close().await;
    });
}

/// A shell keeps an inherited `PWD` that names the directory it starts
/// in, by whatever path, and `cd ..` then goes to the parent of that name.
/// For a workspace reached through a link that is not where the command
/// analysis followed the `cd` to, so every command is told the directory
/// as it was resolved.
#[cfg(unix)]
#[test]
fn sys_a_command_in_a_workspace_behind_a_link_starts_where_it_really_is() {
    let world = World::new(
        "shell_pwd",
        [
            calls(&[("c1", "shell", json!({"command": "cd .. && pwd"}))]),
            says("Seen."),
        ],
    );
    // The workspace is moved somewhere deeper, and a link is left where it
    // was: the harness is started in the link, as a user's shell would be.
    let link = world.workspace.clone();
    let real = world.dir.join("elsewhere").join("deep").join("app");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::rename(&link, &real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let real_parent = std::fs::canonicalize(real.parent().unwrap()).unwrap();

    let core = world.core_with(|options| {
        options.shell_env = Some(world.shell_env(&[("PWD", link.to_str().unwrap())]));
    });
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn_allowing("where is the parent").await;
        let text = shell_text(&front);
        assert_eq!(
            text.lines().next().map(str::trim),
            real_parent.to_str(),
            "the command went to the parent of the link: {text}"
        );
        front.close().await;
    });
}
