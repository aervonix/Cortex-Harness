//! Context management through the core: instruction files read from the
//! disk as the agent would read them, a compaction asked for with
//! `session/compact` and undone by a rewind, and a session resumed from a
//! summary.

use cortex_protocol::methods::{Op, SessionCompactParams, SessionRef, SessionRewindParams};
use cortex_protocol::{Item, Role};
use serde_json::Value;

use crate::{Front, World, says};

fn kinds(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn instruction_files_are_read_as_the_agent_would_read_them() {
    let world = World::new("instructions_disk", [says("Hello.")]);
    world.put("AGENTS.md", "Use tabs.");
    world.put("sub/AGENTS.md", "Nested, not read at the start.");
    let config = world.dirs().config.unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("AGENTS.md"), "The user's own: be brief.").unwrap();
    #[cfg(unix)]
    {
        // A link to a file outside the workspace is not followed: what it
        // holds would go to the provider with every request.
        let outside = world.dir.join("outside-secret.txt");
        std::fs::write(&outside, "NOT-FOR-THE-PROVIDER").unwrap();
        std::os::unix::fs::symlink(&outside, world.workspace.join("CLAUDE.md")).unwrap();
    }
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("go").await;
        front.close().await;
    });
    let system = world.provider.requests()[0].system.clone();
    assert!(system.contains("Use tabs."), "{system}");
    assert!(system.contains("The user's own: be brief."));
    assert!(system.find("be brief").unwrap() < system.find("Use tabs.").unwrap());
    assert!(!system.contains("Nested, not read at the start."));
    assert!(!system.contains("NOT-FOR-THE-PROVIDER"));
}

#[cfg(unix)]
#[test]
fn a_claude_file_that_links_to_agents_is_read_once() {
    let world = World::new("instructions_link_inside", [says("Hello.")]);
    world.put("AGENTS.md", "Use tabs.");
    std::os::unix::fs::symlink("AGENTS.md", world.workspace.join("CLAUDE.md")).unwrap();
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("go").await;
        front.close().await;
    });
    let system = world.provider.requests()[0].system.clone();
    assert_eq!(system.matches("Use tabs.").count(), 1, "{system}");
}

#[test]
fn a_compaction_asked_for_is_undone_by_a_rewind_to_before_it() {
    let world = World::new(
        "compact_and_undo",
        [
            says("First answer."),
            says("The summary."),
            says("After the summary."),
            says("After the rewind."),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("first").await;
        let first_answer = front
            .items()
            .iter()
            .find_map(|item| match item {
                Item::AssistantMessage(message) => Some(message.id.clone()),
                _ => None,
            })
            .unwrap();
        front
            .op(Op::SessionCompact(SessionCompactParams {
                session_id: front.session.clone(),
                focus: Some("the first answer".to_owned()),
            }))
            .await
            .unwrap();
        front.turns(2).await;
        front.turn("second").await;
        let compacted = world.provider.requests()[2].clone();
        assert_eq!(compacted.messages.len(), 2, "the summary and the prompt");
        let summary = crate::texts(&compacted.messages[0]);
        assert!(summary.contains("The summary.") && summary.contains("first"));

        // Back to before the compaction: the epoch it began is off the
        // branch, and the whole conversation is sent again.
        front
            .op(Op::SessionRewind(SessionRewindParams {
                session_id: front.session.clone(),
                to: first_answer,
                restore_files: false,
            }))
            .await
            .unwrap();
        front.turn("third").await;
        let undone = world.provider.requests()[3].clone();
        let said: Vec<String> = undone.messages.iter().map(crate::texts).collect();
        assert_eq!(said.len(), 3, "{said:#?}");
        assert!(said[0].ends_with("first"));
        assert_eq!(said[1], "First answer.");
        assert_eq!(said[2], "third");
        assert_eq!(undone.messages[1].role, Role::Assistant);

        let lines = front.log(&world);
        let kinds = kinds(&lines);
        assert_eq!(kinds.iter().filter(|kind| *kind == "summary").count(), 1);
        assert_eq!(kinds.iter().filter(|kind| *kind == "epoch").count(), 2);
        let summary = lines.iter().find(|line| line["kind"] == "summary").unwrap();
        assert!(summary["carried"]["plan"].is_object());
        assert_eq!(summary["carried"]["last_user"][0]["text"], "first");
        assert!(
            kinds.iter().any(|kind| kind == "usage"),
            "the summary's request is in the log: {kinds:?}"
        );
        front.close().await;
    });
}

#[test]
fn a_session_resumed_after_a_compaction_begins_from_its_summary() {
    let world = World::new(
        "resume_from_summary",
        [says("First answer."), says("The summary."), says("Resumed.")],
    );
    let core = world.core();
    let session = world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("first").await;
        front
            .op(Op::SessionCompact(SessionCompactParams {
                session_id: front.session.clone(),
                focus: None,
            }))
            .await
            .unwrap();
        front.turns(2).await;
        front.close().await;
        front.session.clone()
    });
    // A second run of the program.
    let core = world.core();
    world.run(async {
        let client = core.connect(cortex_core::ClientInfo::user());
        let opened = core
            .handle(
                client.id,
                Op::SessionOpen(SessionRef {
                    session_id: session.clone(),
                }),
            )
            .await
            .unwrap();
        let cortex_protocol::methods::OpResult::SessionOpen(opened) = opened else {
            panic!("not the result of session/open");
        };
        let front = Front {
            core: core.clone(),
            client,
            session: session.clone(),
            state: opened.session,
            seen: std::cell::RefCell::default(),
            answered: std::cell::RefCell::default(),
        };
        front.turn("again").await;
        front.close().await;
    });
    let resumed = world.provider.requests().pop().unwrap();
    let said: Vec<String> = resumed.messages.iter().map(crate::texts).collect();
    assert_eq!(said.len(), 2, "{said:#?}");
    assert!(said[0].contains("The summary."));
    assert_eq!(said[1], "again");
}
