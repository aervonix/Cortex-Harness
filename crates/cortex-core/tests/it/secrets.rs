//! Secrets and the session's files (`docs/security.md`, section 4;
//! `docs/architecture/07-sessions-and-context.md`, "Secrets in sessions").
//!
//! A credential that turns up in a session is masked before anything is
//! written; a provider's own block that holds one is never written at all;
//! and a session that is resumed after either does not send a provider
//! reasoning that no longer matches the history it was written against.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cortex_base::{CancelToken, Secret};
use cortex_core::{ClientInfo, Connector, Core};
use cortex_llm::{
    ImageSource, ModelClient, ModelEvent, ModelRequest, ModelStream, ProviderError, ProviderInfo,
};
use cortex_protocol::event::TurnOutcome;
use cortex_protocol::methods::{Op, OpResult, SessionRef};
use cortex_protocol::{Message, Part, Role, StopReason};
use cortex_testkit::{Response, ScriptedProvider};
use serde_json::{Value, json};

use crate::{Front, World, calls, says};

/// The harness's own key in these tests. Made up.
const KEY: &str = "cortex-core-test-key-3d9f71ac";
/// A key in a well-known format. Made up.
const TOKEN: &str = "ghp_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8";

fn with_key(world: &World) -> Core {
    world.core_with(|options| {
        options
            .api_keys
            .insert("anthropic".to_owned(), Secret::new(KEY));
    })
}

/// Whether any file the harness stored holds `secret`.
fn stored_anywhere(world: &World, secret: &str) -> Vec<String> {
    world
        .stored()
        .into_iter()
        .filter(|(_, content)| {
            content
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        })
        .map(|(path, _)| path.display().to_string())
        .collect()
}

fn text_of(message: &Message) -> String {
    message
        .parts
        .iter()
        .map(|part| match part {
            Part::Text(text) => text.text.clone(),
            Part::ToolResult(result) => result
                .content
                .iter()
                .filter_map(|part| match part {
                    Part::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect(),
            _ => String::new(),
        })
        .collect()
}

fn has_raw(message: &Message) -> bool {
    message.parts.iter().any(|part| match part {
        Part::Text(text) => text.raw.is_some(),
        Part::ToolCall(call) => call.raw.is_some(),
        Part::Reasoning(reasoning) => reasoning.raw.is_some(),
        Part::ProviderBlock(_) => true,
        _ => false,
    })
}

fn resume(world: &World, core: &Core, session: &cortex_protocol::SessionId) -> Front {
    world.run(async {
        let client = core.connect(ClientInfo::user());
        let opened = core
            .handle(
                client.id,
                Op::SessionOpen(SessionRef {
                    session_id: session.clone(),
                }),
            )
            .await
            .expect("the session opens");
        let OpResult::SessionOpen(opened) = opened else {
            panic!("not a session");
        };
        Front {
            core: core.clone(),
            client,
            session: session.clone(),
            state: opened.session,
            seen: std::cell::RefCell::new(Vec::new()),
            answered: std::cell::RefCell::new(Vec::new()),
        }
    })
}

#[test]
fn sys_the_harnesss_own_key_and_a_key_in_a_known_format_are_masked_on_disk() {
    let world = World::new(
        "mask",
        [
            calls(&[(
                "c1",
                "shell",
                json!({"command": format!("echo {KEY} {TOKEN}")}),
            )]),
            says("I saw them."),
        ],
    );
    let core = with_key(&world);
    let session = world.run(async {
        let front = Front::start(&world, &core).await;
        front
            .turn_allowing(&format!("my key is {KEY}, do not lose it"))
            .await;
        // The model saw the output as it was: nothing is masked in the
        // conversation that is running.
        let request = world.provider.requests().pop().unwrap();
        assert!(text_of(&request.messages[0]).contains(KEY));
        assert!(text_of(&request.messages[2]).contains(KEY));
        assert!(text_of(&request.messages[2]).contains(TOKEN));
        // Front ends are shown it as it was too.
        assert!(front.results()[0].2.contains(KEY));
        let log = front.log(&world);
        front.close().await;

        // On disk it is masked, in the prompt and in the result, and each
        // entry that was changed says so.
        let user = log.iter().find(|line| line["kind"] == "user").unwrap();
        assert!(user.to_string().contains("my key is [redacted]"), "{user}");
        assert_eq!(user["redacted"], true);
        let result = log
            .iter()
            .find(|line| line["kind"] == "tool_result")
            .unwrap();
        assert_eq!(result["redacted"], true);
        assert!(
            result.to_string().contains("[redacted] [redacted]"),
            "{result}"
        );
        // An entry with nothing to mask has no mark.
        let last = log
            .iter()
            .rev()
            .find(|line| line["kind"] == "assistant")
            .unwrap();
        assert!(last.get("redacted").is_none(), "{last}");
        front.session.clone()
    });
    assert_eq!(stored_anywhere(&world, KEY), Vec::<String>::new());
    assert_eq!(stored_anywhere(&world, TOKEN), Vec::<String>::new());
    let _ = session;
}

#[test]
fn a_provider_block_that_holds_a_secret_is_never_written_and_the_live_session_keeps_it() {
    let world = World::new(
        "raw_block",
        [
            // The model repeats the key, in its text and in its reasoning:
            // both of the provider's own blocks hold it.
            Response::new()
                .reasoning(&format!("The user's key is {KEY}."))
                .text(&format!("Your key is {KEY}."))
                .end(StopReason::End),
            Response::new()
                .reasoning("Thinking about the next step.")
                .text("Second answer.")
                .end(StopReason::End),
            says("After the resume."),
        ],
    );
    let core = with_key(&world);
    let session = world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("what is my key?").await;
        // The user is told, and the notice does not hold the secret.
        let notices = front.notices();
        let notice = notices
            .iter()
            .find(|notice| notice.contains("stored without its provider blocks"))
            .unwrap_or_else(|| panic!("no notice in {notices:?}"));
        assert!(!notice.contains(KEY));

        // The session that is running still replays the message as the
        // provider sent it: raw blocks, and who produced them.
        front.turn("and then?").await;
        let request = world.provider.requests().pop().unwrap();
        let first = &request.messages[1];
        assert_eq!(first.role, Role::Assistant);
        assert!(has_raw(first), "the live session lost the raw blocks");
        assert!(first.origin.is_some());

        let log = front.log(&world);
        front.close().await;
        // On disk: the message without its provider blocks, marked, and
        // nothing that says a protocol may replay it.
        let stored: Vec<&Value> = log
            .iter()
            .filter(|line| line["kind"] == "assistant")
            .collect();
        assert_eq!(stored[0]["redacted"], true);
        assert!(!stored[0].to_string().contains("\"raw\""), "{}", stored[0]);
        assert!(stored[0]["item"].get("origin").is_none(), "{}", stored[0]);
        assert!(stored[0].to_string().contains("Your key is [redacted]."));
        // The message after it had nothing to hide, and is stored whole.
        assert!(stored[1].get("redacted").is_none());
        assert!(stored[1].to_string().contains("\"raw\""));
        front.session.clone()
    });
    assert_eq!(stored_anywhere(&world, KEY), Vec::<String>::new());

    // Resumed: the altered message is not replayable, and neither is the
    // reasoning that came after it, which was written against a history
    // the log no longer holds.
    let core = with_key(&world);
    let front = resume(&world, &core, &session);
    world.run(async {
        front.turn("go on").await;
        let request = world.provider.requests().pop().unwrap();
        let assistants: Vec<&Message> = request
            .messages
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .collect();
        assert_eq!(assistants.len(), 2);
        for message in &assistants {
            assert!(
                !has_raw(message),
                "a raw block was replayed after the resume"
            );
            assert!(message.origin.is_none());
        }
        // The text is all there, masked where it was masked.
        assert_eq!(text_of(assistants[0]), "Your key is [redacted].");
        assert_eq!(text_of(assistants[1]), "Second answer.");
        // And the user is told, in a line.
        let notices = front.notices();
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("reasoning of 1 earlier response is not sent")),
            "{notices:?}"
        );
        front.close().await;
    });
}

/// A provider that ties each reasoning block to the history before it, as
/// Anthropic's current models do: a block sent back after anything before
/// it has changed is refused.
#[derive(Clone)]
struct Tying {
    script: ScriptedProvider,
    /// For each reasoning block seen, the history it was first sent with.
    written_against: Arc<Mutex<BTreeMap<String, String>>>,
    refused: Arc<Mutex<Vec<String>>>,
}

impl ModelClient for Tying {
    fn stream(&self, request: ModelRequest, cancel: CancelToken) -> ModelStream {
        let mut known = self.written_against.lock().unwrap();
        for (index, message) in request.messages.iter().enumerate() {
            for part in &message.parts {
                let Part::Reasoning(reasoning) = part else {
                    continue;
                };
                let Some(raw) = &reasoning.raw else { continue };
                let before = serde_json::to_string(&request.messages[..index]).unwrap();
                let written = known
                    .entry(raw.as_str().to_owned())
                    .or_insert_with(|| before.clone());
                if *written != before {
                    self.refused.lock().unwrap().push(raw.as_str().to_owned());
                    return ModelStream::from_events(vec![ModelEvent::Error(ProviderError::new(
                        cortex_base::ErrorKind::Provider,
                        "invalid_request_error",
                        "a reasoning block does not match the conversation before it",
                    ))]);
                }
            }
        }
        drop(known);
        self.script.stream(request, cancel)
    }
}

impl Connector for Tying {
    fn connect(
        &self,
        _provider: &ProviderInfo,
        _images: Arc<dyn ImageSource>,
    ) -> Option<Arc<dyn ModelClient>> {
        Some(Arc::new(self.clone()))
    }
}

/// The owner's rule for a resume, shown against a provider that would
/// refuse the request without it: a tool result is masked on disk, the
/// response after it has reasoning, and the session is resumed.
#[test]
fn a_resumed_session_does_not_replay_reasoning_against_a_history_redaction_changed() {
    let world = World::new(
        "tied_reasoning",
        [
            Response::new()
                .reasoning("First, read the notes.")
                .tool_call("c1", "read", &json!({"path": "notes.txt"}))
                .end(StopReason::ToolCalls),
            Response::new()
                .reasoning("The output held a key; I will not repeat it.")
                .text("Done.")
                .end(StopReason::End),
            Response::new()
                .reasoning("Continuing.")
                .text("Carried on.")
                .end(StopReason::End),
        ],
    );
    world.put("notes.txt", format!("deploy with the key {KEY}\n"));
    let tying = Tying {
        script: world.provider.clone(),
        written_against: Arc::default(),
        refused: Arc::default(),
    };
    let core = |world: &World| {
        let tying = tying.clone();
        world.core_with(move |options| {
            options
                .api_keys
                .insert("anthropic".to_owned(), Secret::new(KEY));
            options.connector = Some(Arc::new(tying));
        })
    };
    let first = core(&world);
    let session = world.run(async {
        let front = Front::start(&world, &first).await;
        front.turn_allowing("look around").await;
        assert_eq!(front.ends()[0].outcome, TurnOutcome::Completed);
        front.close().await;
        front.session.clone()
    });
    // The log holds the tool's result masked. (The file's own content is
    // kept exactly, in the checkpoint store, for changed-only re-reads:
    // that store is never redacted, because what it holds must be put
    // back, or compared, byte for byte.)
    assert!(!world.log().contains(KEY));
    assert!(world.log().contains("deploy with the key [redacted]"));

    let second = core(&world);
    let front = resume(&world, &second, &session);
    world.run(async {
        let end = front.turn("carry on").await;
        let refused = tying.refused.lock().unwrap().clone();
        assert_eq!(
            refused,
            Vec::<String>::new(),
            "a reasoning block was replayed against the masked history"
        );
        assert_eq!(end.outcome, TurnOutcome::Completed, "{:?}", end.error);
        assert_eq!(front.last_text(), "Carried on.");
        // What was before the change is replayed as it was: the first
        // response's reasoning was written before the tool's output.
        let request = world.provider.requests().pop().unwrap();
        let assistants: Vec<&Message> = request
            .messages
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .collect();
        assert!(
            has_raw(assistants[0]),
            "reasoning from before the change was dropped too"
        );
        assert!(!has_raw(assistants[1]));
        front.close().await;
    });
}
