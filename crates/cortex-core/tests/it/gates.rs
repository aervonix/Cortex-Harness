//! The permission engine at each of its seams, seen from outside the core
//! (`docs/architecture/06-permissions-and-sandbox.md`): what the loop is
//! told of a call, what a directory walk may read, and what the base
//! state may keep.

use std::path::Path;
use std::process::Command;

use cortex_core::{ClientInfo, Outgoing};
use cortex_protocol::Item;
use cortex_protocol::event::TurnOutcome;
use cortex_protocol::session::SessionSettings;
use cortex_testkit::Response;
use serde_json::{Value, json};

use crate::{Front, World, calls, says};

fn deny(effect: &str, pattern: &str) -> String {
    format!(
        "[[permissions.rules]]\neffect = \"{effect}\"\nmatch = \"{pattern}\"\naction = \"deny\"\n\n"
    )
}

fn asked(front: &Front) -> usize {
    front
        .seen
        .borrow()
        .iter()
        .filter(|outgoing| matches!(outgoing, Outgoing::Request { .. }))
        .count()
}

#[test]
fn a_rule_that_denies_is_a_refusal_with_its_reason_and_the_run_goes_on() {
    let world = World::new(
        "deny_rule",
        [
            calls(&[(
                "c1",
                "write",
                json!({"path": "generated/out.txt", "content": "x\n"}),
            )]),
            says("I will leave it."),
        ],
    );
    world.user_config(&deny("write", "generated/**"));
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let end = front.turn("write there").await;
        assert_eq!(end.outcome, TurnOutcome::Completed);
        assert_eq!(asked(&front), 0, "a denial is not a question");
        let result = &front.results()[0];
        assert_eq!(result.1, "denied");
        assert!(
            result.2.contains("generated"),
            "the reason names the rule: {}",
            result.2
        );
        assert_eq!(world.read("generated/out.txt"), None);
        // A rule's refusal is not "nobody could be asked".
        assert!(core.permission_denials(&front.session).is_empty());
        front.close().await;
    });
}

/// A command the engine finds on its table of commands that only read is
/// not looked at for what it changed; one that writes is.
#[test]
fn sys_a_command_that_only_reads_is_told_to_the_loop_as_changing_nothing() {
    let listing = if cfg!(windows) { "Get-Location" } else { "pwd" };
    let world = World::new(
        "read_only_command",
        [
            calls(&[("c1", "shell", json!({"command": listing}))]),
            calls(&[("c2", "shell", json!({"command": "echo more >> notes.txt"}))]),
            says("Done."),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn_allowing("look, then write").await;
        let payloads: Vec<Value> = front
            .items()
            .into_iter()
            .filter_map(|item| match item {
                Item::ToolResult(result) if result.tool == "shell" => result.payload,
                _ => None,
            })
            .collect();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["exit_code"], 0, "{}", payloads[0]);
        // The loop scheduled the first as read-only, so the tool did not
        // look for changes; the second, it did.
        assert!(payloads[0]["changes"].is_null(), "{}", payloads[0]);
        assert!(payloads[1]["changes"].is_object(), "{}", payloads[1]);
        front.close().await;
    });
}

/// A walk is shown to the engine as a read of the tree it starts in: one
/// that starts outside the workspace is asked about, like a read of a file
/// there, and is not left to the gate on the files it finds.
#[test]
fn a_walk_outside_the_workspace_is_asked_about() {
    let world = World::new("walk_outside", []);
    let beside = world.dir.join("beside");
    std::fs::create_dir_all(&beside).unwrap();
    std::fs::write(beside.join("notes.txt"), "FINDME beside\n").unwrap();
    let beside = beside.to_str().unwrap().to_owned();
    world.provider.push(calls(&[(
        "c1",
        "list",
        json!({"path": beside, "depth": 2}),
    )]));
    world.provider.push(calls(&[(
        "c2",
        "search",
        json!({"pattern": "FINDME", "path": beside}),
    )]));
    world.provider.push(says("Looked."));
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let asked = front.turn_allowing("look beside").await;
        assert_eq!(asked, ["list", "search"]);
        let results = front.results();
        assert!(results[0].2.contains("notes.txt"), "{}", results[0].2);
        assert!(results[1].2.contains("notes.txt"), "{}", results[1].2);
        front.close().await;
    });
}

#[test]
fn a_walk_does_not_return_what_the_read_rules_keep_from_the_agent() {
    let world = World::new(
        "read_gate",
        [
            calls(&[("c1", "search", json!({"pattern": "FINDME"}))]),
            calls(&[("c2", "list", json!({"path": ".", "depth": 3}))]),
            says("Searched."),
        ],
    );
    world.user_config(&deny("read", "private/**"));
    world.put("src/open.txt", "FINDME in the open\n");
    world.put("private/key.txt", "FINDME in private\n");
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("search").await;
        let results = front.results();
        let (search, list) = (&results[0].2, &results[1].2);
        assert!(search.contains("open.txt"), "{search}");
        assert!(
            !search.contains("key.txt"),
            "a denied file was searched: {search}"
        );
        assert!(!search.contains("in private"), "{search}");
        assert!(list.contains("open.txt"), "{list}");
        assert!(
            !list.contains("key.txt"),
            "a denied file was listed: {list}"
        );
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
fn sys_the_base_state_keeps_only_what_the_agent_could_read_unasked() {
    let world = World::new("keep_gate", [says("Hello.")]);
    world.put("tracked.txt", "one\n");
    if !(git(&world.workspace, &["init", "-q"])
        && git(&world.workspace, &["add", "."])
        && git(&world.workspace, &["commit", "-q", "-m", "first"]))
    {
        return;
    }
    // Untracked, after the commit: a file that looks like a secret, and
    // one that does not.
    world.put(".env", "DATABASE_PASSWORD=correct-horse-battery-staple\n");
    world.put("notes.txt", "remember the milk, plainly\n");
    let core = world.core_with(|options| {
        options.base_capture = true;
        options.trust_project = true;
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        front.turn("hello").await;
        front.close().await;
        let log = front.log(&world);
        let base = log
            .iter()
            .find(|line| line["kind"] == "base")
            .expect("the base state was recorded");
        let omitted: Vec<(String, String)> = base["omitted"]
            .as_array()
            .map(|omitted| {
                omitted
                    .iter()
                    .map(|entry| {
                        (
                            entry["path"].as_str().unwrap_or_default().to_owned(),
                            entry["reason"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            omitted.contains(&(".env".to_owned(), "protected".to_owned())),
            "{base}"
        );
    });
    let holds = |text: &str| {
        world.stored().iter().any(|(_, content)| {
            content
                .windows(text.len())
                .any(|window| window == text.as_bytes())
        })
    };
    assert!(
        !holds("correct-horse-battery-staple"),
        "the secret file was kept"
    );
    assert!(
        holds("remember the milk, plainly"),
        "an ordinary file was not kept"
    );
}

#[test]
fn a_spending_cap_reaches_the_front_end_as_the_provider_gave_it() {
    let world = World::new(
        "spend_limit",
        [Response::new().error(cortex_llm::ProviderError::new(
            cortex_base::ErrorKind::Provider,
            "enforced_spend_limit_reached",
            "This workspace has reached its monthly spending limit.",
        ))],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let end = front.turn("hello").await;
        assert_eq!(end.outcome, TurnOutcome::Failed);
        let error = end.error.expect("the turn says why it failed");
        assert_eq!(error.kind, cortex_protocol::ErrorKind::Provider);
        assert_eq!(error.code, "enforced_spend_limit_reached");
        assert_eq!(
            error.message,
            "This workspace has reached its monthly spending limit."
        );
        front.close().await;
    });
}
