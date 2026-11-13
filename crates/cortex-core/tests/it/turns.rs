//! A turn, end to end: a prompt in, the model's tool calls run by the real
//! tools, everything written to the real session log, and the events out
//! (`docs/architecture/03-agent-loop.md`,
//! `docs/architecture/07-sessions-and-context.md`).

use cortex_core::ClientInfo;
use cortex_protocol::event::{Event, TurnOutcome};
use cortex_protocol::methods::{
    Op, OpResult, SessionForkParams, SessionListParams, SessionReadParams, SessionRef,
    SessionRewindParams,
};
use cortex_protocol::{Item, Part};
use serde_json::{Value, json};

use crate::{Front, World, calls, says};

fn log_lines(world: &World) -> Vec<Value> {
    world
        .log()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn kinds(world: &World) -> Vec<String> {
    log_lines(world)
        .iter()
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_turn_runs_the_models_tool_calls_and_records_everything() {
    let world = World::new(
        "turn",
        [
            calls(&[
                (
                    "c1",
                    "write",
                    json!({"path": "notes/todo.md", "content": "- ship it\n"}),
                ),
                ("c2", "read", json!({"path": "notes/todo.md"})),
            ]),
            says("Written and read back."),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        // The workspace as the file system spells it, which on Windows can
        // differ from how the test wrote it (a short name, a drive's case).
        assert!(
            front
                .state
                .workspace
                .replace('\\', "/")
                .ends_with("work/app"),
            "{}",
            front.state.workspace
        );
        let end = front.turn("write a note").await;
        assert_eq!(end.outcome, TurnOutcome::Completed);

        // The real tools ran, in the model's order: the read saw the write.
        assert_eq!(world.read("notes/todo.md").as_deref(), Some("- ship it\n"));
        let results = front.results();
        assert_eq!(results.len(), 2);
        assert_eq!(
            (results[0].0.as_str(), results[0].1.as_str()),
            ("write", "success")
        );
        assert_eq!(results[1].0, "read");
        assert!(results[1].2.contains("- ship it"), "{}", results[1].2);
        assert_eq!(front.last_text(), "Written and read back.");

        // The model was shown the results: the second request has the
        // whole conversation.
        let requests = world.provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), 3);
        // The tools the model is offered are the session's own.
        let offered: Vec<&str> = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        for tool in [
            "read", "list", "search", "write", "edit", "plan", "remember", "shell",
        ] {
            assert!(offered.contains(&tool), "no `{tool}` in {offered:?}");
        }

        // Every event has the seq after the one before it.
        let seqs: Vec<u64> = front.events().iter().map(|event| event.seq).collect();
        assert!(seqs.windows(2).all(|pair| pair[1] > pair[0]), "{seqs:?}");
        front.close().await;
    });

    // The log: the session, its lease, the prompt, the response, the
    // round's checkpoint before its results, the answer and the outcome.
    assert_eq!(
        kinds(&world),
        [
            "session",
            "lease",
            "user",
            "assistant",
            "checkpoint",
            "tool_result",
            "tool_result",
            "assistant",
            "outcome",
        ]
    );
    let lines = log_lines(&world);
    let checkpoint = &lines[4];
    assert_eq!(checkpoint["files"][0]["path"], "notes/todo.md");
    assert_eq!(
        checkpoint["files"][0]["before"],
        Value::Null,
        "it did not exist"
    );
    assert_eq!(lines[8]["ended"], "completed");
    assert_eq!(lines[1]["lease"]["grant"]["level"], "supervised");
}

#[test]
fn a_stored_session_is_listed_resumed_and_carries_on() {
    let world = World::new("resume", [says("First answer."), says("Second answer.")]);
    let core = world.core();
    let session = world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("one").await;
        front.close().await;
        front.session.clone()
    });

    // A second core, as a second run of the program.
    let core = world.core();
    world.run(async {
        let client = core.connect(ClientInfo::user());
        let listed = core
            .handle(
                client.id,
                Op::SessionList(SessionListParams {
                    workspace: Some(world.workspace.to_str().unwrap().to_owned()),
                    limit: None,
                    cursor: None,
                }),
            )
            .await
            .unwrap();
        let OpResult::SessionList(listed) = listed else {
            panic!("not a list");
        };
        assert_eq!(listed.sessions.len(), 1);
        assert_eq!(listed.sessions[0].session_id, session);

        let opened = core
            .handle(
                client.id,
                Op::SessionOpen(SessionRef {
                    session_id: session.clone(),
                }),
            )
            .await
            .unwrap();
        let OpResult::SessionOpen(opened) = opened else {
            panic!("not a session");
        };
        assert_eq!(opened.session.session_id, session);
        let front = Front {
            core: core.clone(),
            client,
            session: session.clone(),
            state: opened.session,
            seen: std::cell::RefCell::new(Vec::new()),
            answered: std::cell::RefCell::new(Vec::new()),
        };
        front.turn("two").await;
        assert_eq!(front.last_text(), "Second answer.");
        // The model was sent the first turn too.
        let request = world.provider.requests().pop().unwrap();
        assert_eq!(request.messages.len(), 3);
        front.close().await;
    });
    // One log, with both turns and a lease for each run.
    let kinds = kinds(&world);
    assert_eq!(kinds.iter().filter(|kind| *kind == "user").count(), 2);
    assert_eq!(kinds.iter().filter(|kind| *kind == "lease").count(), 2);
    // Turn and item ids went on where the first run stopped.
    let lines = log_lines(&world);
    let turns: Vec<&str> = lines
        .iter()
        .filter(|line| line["kind"] == "user")
        .map(|line| line["turn"].as_str().unwrap())
        .collect();
    assert_eq!(turns, ["t_1", "t_2"]);
}

#[test]
fn a_rewind_puts_the_files_and_the_conversation_back() {
    let world = World::new(
        "rewind",
        [
            says("Nothing to do yet."),
            // The model reads the file before it may replace it.
            calls(&[("c1", "read", json!({"path": "a.txt"}))]),
            calls(&[(
                "c2",
                "write",
                json!({"path": "a.txt", "content": "changed\n"}),
            )]),
            says("Changed."),
            says("After the rewind."),
        ],
    );
    world.put("a.txt", "original\n");
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("hello").await;
        let keep = front
            .items()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::AssistantMessage(message) => Some(message.id.clone()),
                _ => None,
            })
            .unwrap();
        front.turn("change it").await;
        assert_eq!(world.read("a.txt").as_deref(), Some("changed\n"));

        let rewound = front
            .op(Op::SessionRewind(SessionRewindParams {
                session_id: front.session.clone(),
                to: keep,
                restore_files: true,
            }))
            .await
            .unwrap();
        assert!(matches!(rewound, OpResult::SessionRewind(_)));
        assert_eq!(world.read("a.txt").as_deref(), Some("original\n"));

        // The loop reads the session again: the next request has the first
        // turn and the new prompt, and nothing of the turn that was undone.
        front.turn("again").await;
        let request = world.provider.requests().pop().unwrap();
        assert_eq!(request.messages.len(), 3);
        assert_eq!(front.last_text(), "After the rewind.");
        front.close().await;
    });
    assert!(kinds(&world).contains(&"rewind".to_owned()));
}

#[test]
fn a_fork_is_a_new_session_with_the_same_conversation() {
    let world = World::new("fork", [says("First."), says("In the fork.")]);
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("one").await;
        let forked = front
            .op(Op::SessionFork(SessionForkParams {
                session_id: front.session.clone(),
                at: None,
            }))
            .await
            .unwrap();
        let OpResult::SessionFork(forked) = forked else {
            panic!("not a session");
        };
        assert_ne!(forked.session.session_id, front.session);
        assert_eq!(forked.session.parent, Some(front.session.clone()));
        // The fork is open on this client, and carries on from the same
        // conversation.
        let fork = Front {
            core: core.clone(),
            client: front.client.clone(),
            session: forked.session.session_id.clone(),
            state: forked.session,
            seen: std::cell::RefCell::new(Vec::new()),
            answered: std::cell::RefCell::new(Vec::new()),
        };
        fork.turn("two").await;
        let request = world.provider.requests().pop().unwrap();
        assert_eq!(request.messages.len(), 3);
        fork.close().await;
        front.close().await;
    });
    assert_eq!(world.logs().len(), 2);
}

#[test]
fn a_session_with_no_log_runs_the_same_and_leaves_nothing_behind() {
    let world = World::new(
        "ephemeral",
        [
            calls(&[("c1", "write", json!({"path": "a.txt", "content": "x\n"}))]),
            says("Done."),
        ],
    );
    let core = world.core();
    world.run(async {
        let client = core.connect(ClientInfo::user());
        let result = core
            .handle(
                client.id,
                Op::SessionNew(cortex_protocol::methods::SessionNewParams {
                    workspace: world.workspace.to_str().unwrap().to_owned(),
                    settings: cortex_protocol::session::SessionSettings::default(),
                    title: None,
                    config: serde_json::Map::new(),
                    ephemeral: true,
                }),
            )
            .await
            .unwrap();
        let OpResult::SessionNew(result) = result else {
            panic!("not a session");
        };
        let front = Front {
            core: core.clone(),
            client,
            session: result.session.session_id.clone(),
            state: result.session,
            seen: std::cell::RefCell::new(Vec::new()),
            answered: std::cell::RefCell::new(Vec::new()),
        };
        let end = front.turn("go").await;
        assert_eq!(end.outcome, TurnOutcome::Completed);
        assert_eq!(world.read("a.txt").as_deref(), Some("x\n"));
        // It can be read, though nothing is stored.
        let read = front
            .op(Op::SessionRead(SessionReadParams {
                session_id: front.session.clone(),
                item_id: None,
                limit: None,
                cursor: None,
            }))
            .await
            .unwrap();
        let OpResult::SessionRead(read) = read else {
            panic!("not items");
        };
        assert_eq!(read.items.len(), 4);
        assert!(matches!(&read.items[0], Item::UserMessage(message)
            if matches!(&message.parts[0], Part::Text(text) if text.text == "go")));
        front.close().await;
    });
    assert!(world.logs().is_empty(), "{:?}", world.logs());
    assert!(!world.sessions_dir().exists());
}

#[test]
fn an_operation_on_a_session_that_is_not_open_is_refused() {
    let world = World::new("not_open", [says("Hi.")]);
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        // Another client has not opened it.
        let other = core.connect(ClientInfo::user());
        let refused = core
            .handle(
                other.id,
                Op::PlanGet(SessionRef {
                    session_id: front.session.clone(),
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "session_not_open");
        // And no such session at all.
        let refused = front
            .op(Op::SessionOpen(SessionRef {
                session_id: cortex_protocol::SessionId::new("s_nosuchsession"),
            }))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "unknown_session");
        // An event of the session reached only the client that has it
        // open.
        front.turn("hello").await;
        assert!(other.queue.is_empty());
        assert!(
            front
                .events()
                .iter()
                .any(|event| matches!(event.event, Event::TurnStarted(_)))
        );
        front.close().await;
    });
}

/// A rewind works on the session's files off the core's thread, and a
/// prompt that arrives meanwhile does not start a turn under it.
#[test]
fn no_turn_starts_while_a_rewind_is_under_way() {
    use std::future::Future as _;
    use std::task::Poll;

    let world = World::new(
        "rewind_race",
        [says("First."), says("Second."), says("After the rewind.")],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("one").await;
        let keep = front
            .items()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::AssistantMessage(message) => Some(message.id.clone()),
                _ => None,
            })
            .unwrap();
        front.turn("two").await;

        // The rewind is begun and, before it is over, a prompt is sent.
        let mut rewind = std::pin::pin!(front.op(Op::SessionRewind(SessionRewindParams {
            session_id: front.session.clone(),
            to: keep,
            restore_files: false,
        })));
        let mut prompt = std::pin::pin!(front.op(Op::TurnStart(
            cortex_protocol::methods::TurnStartParams {
                session_id: front.session.clone(),
                input: vec![cortex_protocol::Part::text("three")],
                delivery: None,
            }
        )));
        let (mut rewound, mut prompted) = (None, None);
        std::future::poll_fn(|cx| {
            if rewound.is_none()
                && let Poll::Ready(result) = rewind.as_mut().poll(cx)
            {
                rewound = Some(result);
            }
            if prompted.is_none()
                && let Poll::Ready(result) = prompt.as_mut().poll(cx)
            {
                // The rewind had begun and had not ended.
                assert!(rewound.is_none(), "the rewind did not leave the thread");
                prompted = Some(result);
            }
            if rewound.is_some() && prompted.is_some() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        assert!(matches!(rewound, Some(Ok(OpResult::SessionRewind(_)))));
        let refused = prompted
            .unwrap()
            .expect_err("a turn started under a rewind");
        assert_eq!(refused.code, "history_changing");

        // When it is over, a prompt is a prompt again, on the history the
        // rewind left.
        front.turn("again").await;
        assert_eq!(front.last_text(), "After the rewind.");
        assert_eq!(world.provider.requests().pop().unwrap().messages.len(), 3);
        front.close().await;
    });
}
