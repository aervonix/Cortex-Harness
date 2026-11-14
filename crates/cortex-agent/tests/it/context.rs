//! Context management as the loop does it
//! (`docs/architecture/07-sessions-and-context.md`): epochs and the prefix,
//! the system prompt, instruction files, a change of model, background and
//! foreground compaction, and what a compaction carries.
//!
//! The budget is moved with the usage a scripted response reports, or with
//! a small window and a large notice. Nothing here is slow.

use std::sync::Arc;

use cortex_agent::{
    CORE_INSTRUCTIONS, ContextSettings, EpochReason, EpochStart, MemorySessionStore, NOTICE_LEAD,
    RoleModel, SUMMARY_INSTRUCTIONS, check_prefix,
};
use cortex_base::{CancelToken, ContentHash};
use cortex_llm::{
    Catalog, ModelClient, ModelEntry, ModelRequest, ModelStream, Protocol, ProviderInfo,
};
use cortex_protocol::event::{Notice, TurnOutcome};
use cortex_protocol::item::{NoticeSource, PlanItem, PlanStatus, UserMessageItem};
use cortex_protocol::model::ModelRole;
use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{
    Item, ItemId, Message, ModelId, Part, Plan, ProviderId, Role, StopReason, TurnId,
};
use cortex_testkit::{Gate, Response, ScriptedProvider, fixtures};
use serde_json::json;

use crate::{
    DATE, Rig, Setup, World, assert_valid_history, calls, last_request, says, settle, text_of,
};

/// A response that reports this many input tokens.
fn counted(response: Response, input: u64) -> Response {
    response.tokens(TokenUsage {
        input,
        output: 100,
        ..TokenUsage::default()
    })
}

fn has_reasoning(messages: &[Message]) -> bool {
    messages.iter().any(|message| {
        message
            .parts
            .iter()
            .any(|part| matches!(part, Part::Reasoning(_)))
    })
}

fn all_text(message: &Message) -> String {
    text_of(message)
}

fn notice_texts(rig: &Rig) -> Vec<String> {
    rig.store
        .items()
        .into_iter()
        .filter_map(|item| match item {
            Item::Notice(notice) => Some(notice.text),
            _ => None,
        })
        .collect()
}

fn root_file(rig: &Rig, name: &str) -> cortex_base::AbsPath {
    rig.workspace.paths.root().join(name).unwrap()
}

// ---------------------------------------------------------------- prefix

/// The scripted provider, whose stable prefix is what a real protocol
/// would write: the loop's debug-build check then runs on real bytes.
struct Rendered {
    inner: ScriptedProvider,
    protocol: Box<dyn Protocol>,
    provider: ProviderInfo,
    model: Arc<ModelEntry>,
}

impl Rendered {
    fn render(&self, request: &ModelRequest) -> Vec<u8> {
        let mut request = request.clone();
        request.model = Arc::clone(&self.model);
        self.protocol
            .stable_prefix(&request, &self.provider)
            .expect("the request renders")
    }
}

impl ModelClient for Rendered {
    fn stream(&self, request: ModelRequest, cancel: CancelToken) -> ModelStream {
        self.inner.stream(request, cancel)
    }

    fn stable_prefix(&self, request: &ModelRequest) -> Option<Vec<u8>> {
        Some(self.render(request))
    }
}

fn anthropic(inner: &ScriptedProvider) -> Rendered {
    let catalog = Catalog::builtin().unwrap();
    Rendered {
        inner: inner.clone(),
        protocol: Box::new(cortex_llm::anthropic::AnthropicMessages),
        provider: cortex_llm::anthropic::provider(),
        model: Arc::clone(
            catalog
                .get(
                    &ProviderId::new("anthropic"),
                    &ModelId::new("claude-haiku-5-5"),
                )
                .unwrap(),
        ),
    }
}

fn openai_chat(inner: &ScriptedProvider) -> Rendered {
    let catalog = Catalog::builtin().unwrap();
    Rendered {
        inner: inner.clone(),
        protocol: Box::new(cortex_llm::openai_chat::OpenAiChat),
        provider: cortex_llm::openai_chat::providers::builtin("deepseek")
            .unwrap()
            .info,
        model: Arc::clone(
            catalog
                .get(
                    &ProviderId::new("deepseek"),
                    &ModelId::new("deepseek-flash"),
                )
                .unwrap(),
        ),
    }
}

#[test]
fn the_prefix_holds_over_a_long_session_and_breaks_when_history_is_edited() {
    let world = World::new(
        "long_prefix",
        [
            calls(&[
                ("c1", "read", json!({"path": "a", "gate": "g1"})),
                ("c2", "peek", json!({"path": "b"})),
            ]),
            Response::new()
                .reasoning("Thinking.")
                .text("Read both.")
                .end(StopReason::End),
            calls(&[("c3", "write", json!({"path": "c"}))]),
            Response::new()
                .text("One more.")
                .tool_call("c4", "shell", &json!({"id": "x"}))
                .end(StopReason::ToolCalls),
            says("Done."),
            says("And the last."),
        ],
    );
    let mut setup = Setup::new(&world);
    // The loop's own check renders every request as Anthropic's API is
    // sent it.
    setup.model = RoleModel::new(Arc::new(anthropic(&world.provider)), fixtures::model());
    let rig = setup.start(&world);
    rig.instructions
        .set(&root_file(&rig, "AGENTS.md"), "Use tabs.");
    world.run(async {
        rig.prompt("go");
        // A steering message arrives while the round runs.
        world.log.has("start read a").await;
        rig.steer("and look at b too");
        world.gates.open("g1");
        rig.events.turns(1).await;
        // Between turns: a notice, and a changed instruction file.
        rig.handle
            .notice(NoticeSource::Plan, "The user added a task.");
        rig.instructions
            .set(&root_file(&rig, "AGENTS.md"), "Use spaces.");
        rig.prompt("now write c");
        rig.events.turns(2).await;
        rig.prompt("and finish");
        rig.events.turns(3).await;
    });
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 6);
    assert!(
        rig.events
            .ends()
            .iter()
            .all(|end| end.outcome == TurnOutcome::Completed)
    );
    // One epoch throughout: the prompt and the tools never changed.
    assert_eq!(rig.store.epochs().len(), 1);
    assert!(
        requests
            .windows(2)
            .all(|pair| pair[0].system == pair[1].system)
    );
    // The notices are in the conversation, each saying whose it is.
    let last = requests.last().unwrap();
    let notes: Vec<&Message> = last
        .messages
        .iter()
        .filter(|message| all_text(message).starts_with(NOTICE_LEAD))
        .collect();
    assert_eq!(notes.len(), 2, "the plan and the changed file");
    assert!(
        notes
            .iter()
            .any(|note| all_text(note).contains("Use spaces."))
    );

    for client in [anthropic(&world.provider), openai_chat(&world.provider)] {
        let bytes: Vec<Vec<u8>> = requests
            .iter()
            .map(|request| client.render(request))
            .collect();
        for (index, pair) in bytes.windows(2).enumerate() {
            assert_eq!(
                check_prefix(&pair[0], &pair[1]),
                Ok(()),
                "{}: request {} does not begin with request {index}",
                client.protocol.name(),
                index + 1
            );
        }
        // An edit to a message already sent is found, where it is.
        let mut edited = requests[4].clone();
        let answer = edited
            .messages
            .iter_mut()
            .skip(1)
            .find(|message| matches!(message.parts.first(), Some(Part::Text(_))))
            .expect("an earlier message with text");
        let Some(Part::Text(text)) = answer.parts.first_mut() else {
            unreachable!()
        };
        text.text.push('!');
        let at = check_prefix(&bytes[3], &client.render(&edited))
            .expect_err("an edited history is not a prefix");
        assert!(at < bytes[3].len());
        // And so is a changed system prompt.
        let mut system = requests[4].clone();
        system.system.push_str(" More.");
        assert!(check_prefix(&bytes[3], &client.render(&system)).is_err());
    }
}

// ---------------------------------------------------------------- prompt

#[test]
fn the_system_prompt_has_its_sections_and_the_project_instructions() {
    let world = World::new("prompt_sections", [says("Hello.")]);
    let mut setup = Setup::new(&world);
    let user = cortex_base::AbsPath::from_os(&world.dir.join("config")).unwrap();
    setup.user_dir = Some(user.clone());
    let rig = setup.start(&world);
    rig.instructions
        .set(&user.join("AGENTS.md").unwrap(), "Be brief.");
    rig.instructions
        .set(&root_file(&rig, "AGENTS.md"), "Use tabs.");
    rig.instructions
        .set(&root_file(&rig, "CLAUDE.md"), "Write tests.");
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let request = last_request(&world.provider);
    let system = &request.system;
    assert!(system.starts_with(CORE_INSTRUCTIONS));
    let at = |text: &str| {
        system
            .find(text)
            .unwrap_or_else(|| panic!("{text} in {system}"))
    };
    assert!(at("Be brief.") < at("Use tabs.") && at("Use tabs.") < at("Write tests."));
    assert!(at("## AGENTS.md") < at("## CLAUDE.md"));
    assert_eq!(
        text_of(&request.messages[0]),
        format!("{DATE}go"),
        "the date is in the first user message, not the prompt"
    );
    assert!(!system.contains("2026-11-13"));
    let epoch = &rig.store.epochs()[0];
    assert_eq!(epoch.reason, Some(EpochReason::Start));
    assert_eq!(epoch.instructions.len(), 3);
    assert_eq!(epoch.system, *system);
}

// --------------------------------------------------- instruction files

#[test]
fn changed_new_and_removed_instruction_files_are_told_as_notices() {
    let world = World::new(
        "instructions_change",
        [says("One."), says("Two."), says("Three."), says("Four.")],
    );
    let rig = Setup::new(&world).start(&world);
    let agents = root_file(&rig, "AGENTS.md");
    rig.instructions.set(&agents, "Use tabs.");
    let turn = |text: &str, count: usize| {
        world.run(async {
            rig.prompt(text);
            rig.events.turns(count).await;
        });
    };
    turn("first", 1);
    let system = last_request(&world.provider).system;
    assert_eq!(notice_texts(&rig).len(), 0);

    rig.instructions.set(&agents, "Use spaces.");
    turn("second", 2);
    rig.instructions
        .set(&root_file(&rig, "CLAUDE.md"), "Write tests.");
    turn("third", 3);
    rig.instructions.remove(&agents);
    turn("fourth", 4);

    let notices = notice_texts(&rig);
    assert_eq!(notices.len(), 3, "{notices:#?}");
    assert!(notices[0].starts_with("Instruction file `AGENTS.md` changed."));
    assert!(notices[0].ends_with("Use spaces."));
    assert!(notices[1].starts_with("Instruction file `CLAUDE.md` is new."));
    assert!(notices[2].starts_with("Instruction file `AGENTS.md` was removed."));
    // The system prompt is not rewritten within the epoch.
    assert!(
        world
            .provider
            .requests()
            .iter()
            .all(|request| request.system == system)
    );
    assert!(system.contains("Use tabs.") && !system.contains("Use spaces."));
    assert_eq!(rig.store.epochs().len(), 1);
    // Front ends are told too.
    let told = rig
        .events
        .notices()
        .into_iter()
        .filter(|notice| matches!(notice, Notice::Instructions(_)))
        .count();
    assert_eq!(told, 3);
}

#[test]
fn a_nested_instruction_file_is_given_once_when_a_tool_first_touches_its_directory() {
    let world = World::new(
        "instructions_nested",
        [
            calls(&[(
                "c1",
                "read",
                json!({"path": "src/api/handler.rs", "record": true}),
            )]),
            calls(&[(
                "c2",
                "read",
                json!({"path": "src/api/routes.rs", "record": true}),
            )]),
            says("Done."),
        ],
    );
    let rig = Setup::new(&world).start(&world);
    rig.instructions
        .set(&root_file(&rig, "src/api/AGENTS.md"), "Return JSON errors.");
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let requests = world.provider.requests();
    assert!(!requests[0].system.contains("Return JSON errors."));
    let nested: Vec<String> = notice_texts(&rig);
    assert_eq!(
        nested.len(),
        1,
        "once, though two files of it were read: {nested:#?}"
    );
    assert!(
        nested[0].starts_with(
            "Instruction file `src/api/AGENTS.md` applies to the files under `src/api/`"
        )
    );
    assert!(nested[0].ends_with("Return JSON errors."));
    // It comes after the round that touched the directory, before the
    // request that follows it.
    let second = &requests[1];
    let last = second.messages.last().unwrap();
    assert!(all_text(last).contains("Return JSON errors."));
    assert_valid_history(&second.messages);
}

#[test]
fn instruction_files_over_the_cap_are_cut_and_the_user_is_told() {
    let world = World::new("instructions_cap", [says("Hello.")]);
    let mut setup = Setup::new(&world);
    setup.context = ContextSettings {
        instructions_cap: 1_000,
        ..ContextSettings::default()
    };
    let rig = setup.start(&world);
    rig.instructions
        .set(&root_file(&rig, "AGENTS.md"), "a".repeat(800));
    rig.instructions
        .set(&root_file(&rig, "CLAUDE.md"), "c".repeat(800));
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let system = last_request(&world.provider).system;
    assert!(system.contains(&"a".repeat(800)));
    assert!(system.contains(&"c".repeat(200)) && !system.contains(&"c".repeat(201)));
    assert!(system.contains("The rest of this file was left out"));
    let warned: Vec<String> = rig
        .events
        .notices()
        .into_iter()
        .filter_map(|notice| match notice {
            Notice::Instructions(notice) => Some(notice.message),
            _ => None,
        })
        .collect();
    assert_eq!(warned.len(), 1);
    assert!(warned[0].contains("the end of CLAUDE.md"), "{warned:?}");
}

// ------------------------------------------------------- a change of model

#[test]
fn a_change_of_model_starts_an_epoch_that_keeps_the_prompt_and_tells_the_seen_files() {
    let world = World::new(
        "model_epoch",
        [
            calls(&[("c1", "read", json!({"path": "a", "record": true}))]),
            Response::new()
                .reasoning("Mine.")
                .text("Read.")
                .end(StopReason::End),
            says("On the other model."),
        ],
    );
    let rig = Setup::new(&world).start(&world);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let file = root_file(&rig, "a");
    assert!(rig.workspace.files.shown(&file).is_some());
    let other = RoleModel::new(Arc::new(world.provider.clone()), fixtures::plain_model());
    rig.handle.set_role_model(ModelRole::Main, other);
    world.run(async {
        rig.prompt("again");
        rig.events.turns(2).await;
    });
    let epochs = rig.store.epochs();
    assert_eq!(epochs.len(), 2);
    assert_eq!(epochs[1].reason, Some(EpochReason::Model));
    assert_eq!(epochs[1].model.as_str(), fixtures::PLAIN_MODEL);
    assert_eq!(
        (&epochs[0].system, &epochs[0].tools),
        (&epochs[1].system, &epochs[1].tools)
    );
    // The history goes on as it was, reasoning and all: the provider says
    // what the new model may read.
    let request = last_request(&world.provider);
    assert!(has_reasoning(&request.messages));
    // The model no longer has what it was shown: the next read is whole.
    assert!(rig.workspace.files.shown(&file).is_none());
    assert!(
        rig.workspace.files.snapshot(&file).is_some(),
        "the snapshot stays"
    );
}

// ---------------------------------------------------------- the background

/// A session whose `fast` role has a provider of its own.
fn with_fast(world: &World, fast: &ScriptedProvider) -> Rig {
    let mut setup = Setup::new(world);
    setup.utility = Some(RoleModel::new(Arc::new(fast.clone()), fixtures::model()));
    setup.start(world)
}

/// Three requests of one turn: the first counted past the threshold for a
/// summary in the background, the second past the trigger.
fn filling() -> Vec<Response> {
    vec![
        counted(calls(&[("c1", "read", json!({"path": "a"}))]), 140_000),
        counted(
            Response::new()
                .reasoning("Second thoughts.")
                .text("Reading b.")
                .tool_call("c2", "read", &json!({"path": "b"}))
                .end(StopReason::ToolCalls),
            165_000,
        ),
        says("Done."),
    ]
}

fn summary_of(provider: &ScriptedProvider) -> Vec<ModelRequest> {
    provider.requests()
}

#[test]
fn a_summary_prepared_in_the_background_is_used_and_the_newer_turns_follow_it_without_reasoning() {
    let world = World::new("background_used", filling());
    let fast = ScriptedProvider::new([says("Goal: read a and b.")]);
    let rig = with_fast(&world, &fast);
    world.run(async {
        rig.prompt("read them");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    // The summary was asked of the fast role, once, and never of the main
    // model.
    let summaries = summary_of(&fast);
    assert_eq!(
        summaries.len(),
        1,
        "{:#?}",
        summaries
            .iter()
            .map(|request| request.messages.iter().map(all_text).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
    assert_eq!(summaries[0].role, ModelRole::Fast);
    assert!(all_text(summaries[0].messages.last().unwrap()).contains(SUMMARY_INSTRUCTIONS));
    let main = world.provider.requests();
    assert_eq!(
        main.len(),
        3,
        "the session went on: no request of its own for the summary"
    );

    // The new epoch: the summary, then the turns after it was taken, with
    // their text, calls and results and without their reasoning.
    let third = &main[2];
    assert!(all_text(&third.messages[0]).contains("Goal: read a and b."));
    assert!(
        all_text(&third.messages[0]).contains("read them"),
        "the last user message"
    );
    assert!(!has_reasoning(&third.messages));
    let texts: Vec<String> = third.messages.iter().map(all_text).collect();
    assert!(texts.iter().any(|text| text == "Reading b."), "{texts:#?}");
    assert_eq!(third.messages.last().unwrap().role, Role::Tool);
    assert_valid_history(&third.messages);
    assert_eq!(
        third.messages.len(),
        3,
        "the summary, and the round after it"
    );
    // In the log: the summary, then the epoch; the summary's usage.
    let epochs = rig.store.epochs();
    assert_eq!(epochs.len(), 2);
    assert_eq!(epochs[1].reason, Some(EpochReason::Compaction));
    assert_eq!(rig.store.summaries().len(), 1);
    assert_eq!(rig.store.out_of_band().len(), 1);
    assert_eq!(rig.store.out_of_band()[0].1.role, ModelRole::Fast);
    // Front ends were told it was prepared in the background.
    let background = rig
        .events
        .notices()
        .into_iter()
        .filter(|notice| matches!(notice, Notice::Compaction(compaction) if compaction.background))
        .count();
    assert_eq!(background, 1);
}

#[test]
fn the_trigger_waits_for_a_summary_still_under_way() {
    let world = World::new("background_waited", filling());
    let gate = Gate::new();
    let fast = ScriptedProvider::new([Response::new()
        .wait(&gate)
        .text("Goal: read.")
        .end(StopReason::End)]);
    let rig = with_fast(&world, &fast);
    world.run(async {
        rig.prompt("read them");
        // The loop reaches the trigger and waits for the summary.
        while rig.handle.turn_state() != cortex_agent::TurnState::Compacting {
            settle().await;
        }
        settle().await;
        assert_eq!(rig.handle.turn_state(), cortex_agent::TurnState::Compacting);
        assert_eq!(world.provider.requests().len(), 2, "nothing more was sent");
        gate.open();
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert_eq!(fast.requests().len(), 1);
    assert_eq!(world.provider.requests().len(), 3);
    assert!(all_text(&world.provider.requests()[2].messages[0]).contains("Goal: read."));
}

#[test]
fn a_summary_that_failed_in_the_background_is_asked_for_again_at_the_trigger() {
    let world = World::new("background_failed", filling());
    let fast = ScriptedProvider::new([
        Response::new().error(cortex_llm::error::ProviderError::new(
            cortex_base::ErrorKind::Provider,
            "invalid_request",
            "No.",
        )),
        says("Goal: the second try."),
    ]);
    let rig = with_fast(&world, &fast);
    world.run(async {
        rig.prompt("read them");
        rig.events.turns(1).await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
    assert_eq!(fast.requests().len(), 2);
    let third = &world.provider.requests()[2];
    assert!(all_text(&third.messages[0]).contains("Goal: the second try."));
    // Asked for now, it covers everything: nothing is replayed after it.
    assert_eq!(third.messages.len(), 1);
}

#[test]
fn when_every_summary_fails_the_turn_ends_and_nothing_is_recorded() {
    let world = World::new("summary_fails", filling());
    let fast = ScriptedProvider::new([]);
    let rig = with_fast(&world, &fast);
    world.run(async {
        rig.prompt("read them");
        rig.events.turns(1).await;
    });
    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    let error = end.error.as_ref().unwrap();
    assert_eq!(error.code, "compaction_failed");
    assert_eq!(error.kind, cortex_protocol::ErrorKind::ContextOverflow);
    assert!(rig.store.summaries().is_empty());
    assert_eq!(rig.store.epochs().len(), 1);
    assert_eq!(world.provider.requests().len(), 2);
}

#[test]
fn a_summary_over_history_a_rewind_took_away_is_not_used() {
    let world = World::new("background_rewound", [says("First."), says("Second.")]);
    let gate = Gate::new();
    let fast = ScriptedProvider::new([
        Response::new()
            .wait(&gate)
            .text("STALE")
            .end(StopReason::End),
        says("FRESH"),
    ]);
    let rig = with_fast(&world, &fast);
    // Past the threshold for a summary in the background, not the trigger.
    rig.window(4_000);
    rig.bulk(4_600);
    world.run(async {
        rig.prompt("one");
        rig.events.turns(1).await;
        // A summary is under way, over the history as it is now.
        for _ in 0..8 {
            settle().await;
        }
        let estimates: Vec<(u64, u64)> = rig
            .stats
            .all()
            .into_iter()
            .filter_map(|event| match event {
                cortex_agent::StatEvent::Context(context) => {
                    Some((context.estimate, context.limit))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            fast.requests().len(),
            1,
            "estimates and limits: {estimates:?}"
        );
        // The history changes under the loop (a rewind), and only then is
        // the summary ready.
        let mut change = rig.handle.change_history().unwrap();
        change.made();
        drop(change);
        gate.open();
        settle().await;
        // More bulk: the next request is past the trigger.
        rig.bulk(1_500);
        rig.prompt("two");
        rig.events.turns(2).await;
    });
    let second = &world.provider.requests()[1];
    let first = all_text(&second.messages[0]);
    assert!(
        first.contains("FRESH") && !first.contains("STALE"),
        "{first}"
    );
    assert_eq!(fast.requests().len(), 2);
}

#[test]
fn an_interrupt_stops_a_summary_running_in_the_background() {
    let world = World::new(
        "background_interrupted",
        [
            counted(
                calls(&[("c1", "read", json!({"path": "a", "gate": "g"}))]),
                140_000,
            ),
            // The next request is in flight when the interrupt comes.
            Response::events([]).stall(),
        ],
    );
    let gate = Gate::new();
    let fast = ScriptedProvider::new([Response::new()
        .wait(&gate)
        .text("Unused.")
        .end(StopReason::End)]);
    let rig = with_fast(&world, &fast);
    world.run(async {
        rig.prompt("go");
        world.log.has("start read a").await;
        // The round runs; after it the summary would be started.
        world.gates.open("g");
        while fast.requests().is_empty() {
            settle().await;
        }
        let _ = rig.handle.interrupt();
        rig.events.turns(1).await;
        gate.open();
        settle().await;
    });
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Interrupted);
    assert!(
        rig.store.out_of_band().is_empty(),
        "nothing of it was counted as done"
    );
}

// --------------------------------------------------- what is carried

#[test]
fn a_compaction_carries_the_plan_the_files_and_the_last_message_exactly() {
    let world = World::new(
        "carried",
        [
            calls(&[
                ("c1", "read", json!({"path": "src/lib.rs", "record": true})),
                ("c2", "write", json!({"path": "notes.md"})),
            ]),
            says("Turn one is done."),
            says("A summary."),
            says("Carried on."),
        ],
    );
    let plan = Plan {
        items: vec![
            PlanItem {
                id: cortex_protocol::ids::PlanItemId::new("p1"),
                text: "Read the code".to_owned(),
                status: PlanStatus::Done,
            },
            PlanItem {
                id: cortex_protocol::ids::PlanItemId::new("p2"),
                text: "Fix `count`".to_owned(),
                status: PlanStatus::Active,
            },
        ],
        revision: 3,
    };
    let mut setup = Setup::new(&world);
    setup.plan = Some(Arc::new(cortex_tools::PlanBoard::with_plan(plan.clone())));
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("Fix the off-by-one in `count`, please.");
        rig.events.turns(1).await;
    });
    // A changed file the write tool did not see: a command's.
    rig.workspace
        .files
        .note_changed(&root_file(&rig, "build.log"));
    rig.handle
        .compact(Some("the failing test".to_owned()))
        .unwrap();
    world.run(async {
        rig.events.turns(2).await;
    });
    assert_eq!(rig.events.ends()[1].outcome, TurnOutcome::Completed);
    let summaries = rig.store.summaries();
    assert_eq!(summaries.len(), 1);
    let carried = &summaries[0].carried;
    assert_eq!(
        serde_json::from_value::<Plan>(carried["plan"].clone()).unwrap(),
        plan
    );
    let files: Vec<(String, bool, bool)> = carried["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().to_owned(),
                file["read"].as_bool().unwrap(),
                file["changed"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        files,
        [
            ("build.log".to_owned(), false, true),
            ("src/lib.rs".to_owned(), true, false),
        ],
        "the probe's write records nothing: only a real tool's does"
    );
    assert_eq!(
        carried["last_user"][0]["text"],
        "Fix the off-by-one in `count`, please."
    );
    let text = &summaries[0].item.text;
    assert!(text.contains("Fix the off-by-one in `count`, please."));
    assert!(text.contains("[active] Fix `count` (p2)"));
    assert!(text.contains("Today's date is 2026-11-13 (UTC)."));
    // The focus was asked for, and nothing else was sent in the turn.
    let summary_request = &world.provider.requests()[2];
    assert!(all_text(summary_request.messages.last().unwrap()).ends_with("the failing test"));
    assert_eq!(world.provider.requests().len(), 3);
    // The next turn is in the new epoch.
    world.run(async {
        rig.prompt("go on");
        rig.events.turns(3).await;
    });
    let next = last_request(&world.provider);
    assert!(all_text(&next.messages[0]).starts_with(NOTICE_LEAD));
    assert_eq!(next.messages.len(), 2);
}

#[test]
fn a_summary_too_large_for_the_new_epoch_fails_the_compaction_and_records_nothing() {
    let world = World::new(
        "summary_too_large",
        [Response::new()
            .text(&"s".repeat(20_000))
            .end(StopReason::End)],
    );
    let rig = Setup::new(&world).start(&world);
    rig.window(4_000);
    rig.bulk(6_000);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    assert!(
        end.error
            .as_ref()
            .unwrap()
            .message
            .contains("more than a new context")
    );
    assert!(rig.store.summaries().is_empty());
    assert_eq!(rig.store.epochs().len(), 1);
}

#[test]
fn with_automatic_compaction_off_the_turn_stops_and_asks() {
    let world = World::new("auto_off", [says("Unreached.")]);
    let mut setup = Setup::new(&world);
    setup.context = ContextSettings {
        auto_compact: false,
        ..ContextSettings::default()
    };
    let rig = setup.start(&world);
    rig.window(4_000);
    rig.bulk(6_000);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let end = &rig.events.ends()[0];
    assert_eq!(end.outcome, TurnOutcome::Failed);
    let error = end.error.as_ref().unwrap();
    assert_eq!(error.code, "compaction_needed");
    assert!(error.message.contains("session/compact"));
    assert!(world.provider.requests().is_empty(), "nothing was sent");
}

#[test]
fn recent_turns_can_be_replayed_after_the_summary_without_their_reasoning() {
    let world = World::new(
        "keep_recent",
        [
            Response::new()
                .reasoning("Old.")
                .text("Old answer.")
                .end(StopReason::End),
            Response::new()
                .reasoning("New.")
                .text("New answer.")
                .end(StopReason::End),
            says("A summary."),
            says("After."),
        ],
    );
    let mut setup = Setup::new(&world);
    setup.context = ContextSettings {
        // About the last answer, at the default ratio.
        keep_recent_tokens: 7,
        ..ContextSettings::default()
    };
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("first question");
        rig.events.turns(1).await;
        rig.prompt("second question");
        rig.events.turns(2).await;
    });
    rig.handle.compact(None).unwrap();
    world.run(async {
        rig.events.turns(3).await;
        rig.prompt("third");
        rig.events.turns(4).await;
    });
    let request = last_request(&world.provider);
    let texts: Vec<String> = request.messages.iter().map(all_text).collect();
    assert!(texts.iter().any(|text| text == "New answer."), "{texts:#?}");
    assert!(
        !texts.iter().any(|text| text == "Old answer."),
        "{texts:#?}"
    );
    assert!(!has_reasoning(&request.messages));
    // The summary request covered only what is not replayed.
    let summary_request = &world.provider.requests()[2];
    let covered: Vec<String> = summary_request.messages.iter().map(all_text).collect();
    assert!(covered.iter().any(|text| text == "Old answer."));
    assert!(!covered.iter().any(|text| text == "New answer."));
    assert_valid_history(&request.messages);
}

// ------------------------------------------------------------ resuming

fn resumed(world: &World, system: &str) -> (Rig, MemorySessionStore) {
    let turn = TurnId::new("t_1");
    let store = MemorySessionStore::with_items(
        &turn,
        [
            Item::UserMessage(UserMessageItem {
                id: ItemId::new("i_1"),
                parts: vec![Part::text("before")],
                delivery: None,
                created_at: Some("2026-11-12T09:00:00Z".into()),
            }),
            Item::AssistantMessage(cortex_protocol::item::AssistantMessageItem {
                id: ItemId::new("i_2"),
                parts: vec![
                    Part::Reasoning(cortex_protocol::message::ReasoningPart {
                        text: "Then.".to_owned(),
                        raw: Some(cortex_protocol::RawJson::new(r#"{"signature":"s"}"#)),
                    }),
                    Part::text("Earlier answer."),
                ],
                stop_reason: Some(StopReason::End),
                raw_stop: None,
                stop_detail: None,
                truncated: false,
                origin: None,
                usage: None,
                error: None,
                created_at: None,
            }),
        ],
    );
    // The epoch it was in, put first.
    let mut records = store.records();
    records.insert(
        0,
        (
            turn.clone(),
            cortex_agent::Record::Epoch(EpochStart {
                provider: ProviderId::new(fixtures::PROVIDER),
                protocol: fixtures::PROTOCOL.to_owned(),
                model: ModelId::new(fixtures::MODEL),
                effort: None,
                system: system.to_owned(),
                tools: Vec::new(),
                reason: Some(EpochReason::Start),
                base: None,
                instructions: vec![("AGENTS.md".to_owned(), ContentHash::of(b"x"))],
                altered: false,
            }),
        ),
    );
    let fresh = MemorySessionStore::new();
    {
        let mut writer = fresh.clone();
        for (turn, record) in records {
            cortex_testkit::block_on(cortex_agent::SessionStore::append(
                &mut writer,
                &turn,
                record,
            ))
            .unwrap();
        }
    }
    let mut setup = Setup::new(world);
    setup.memory = fresh.clone();
    (setup.start(world), fresh)
}

#[test]
fn a_resumed_session_whose_prompt_is_not_the_harnesss_now_begins_an_epoch_without_old_reasoning() {
    let world = World::new("resume_new_epoch", [says("Now.")]);
    let (rig, store) = resumed(&world, "An old prompt.");
    world.run(async {
        rig.prompt("after");
        rig.events.turns(1).await;
    });
    let epochs = store.epochs();
    assert_eq!(epochs.len(), 2);
    assert_eq!(epochs[1].reason, Some(EpochReason::Resume));
    let request = last_request(&world.provider);
    assert!(request.system.starts_with(CORE_INSTRUCTIONS));
    assert!(
        !has_reasoning(&request.messages),
        "produced under the old prompt"
    );
    // The epoch's first user message carries the date it was written on.
    assert_eq!(
        text_of(&request.messages[0]),
        "Today's date is 2026-11-12 (UTC).before"
    );
    assert_eq!(all_text(&request.messages[1]), "Earlier answer.");
    assert_eq!(rig.events.ends()[0].outcome, TurnOutcome::Completed);
}

#[test]
fn a_resumed_session_goes_on_in_its_epoch_when_the_prompt_is_the_same() {
    // The prompt a session of this rig gets, taken from a first session.
    let world = World::new("resume_same_epoch", [says("One."), says("Two.")]);
    let probe = Setup::new(&world).start(&world);
    world.run(async {
        probe.prompt("probe");
        probe.events.turns(1).await;
    });
    let epoch = probe.store.epochs().remove(0);
    let (rig, store) = resumed(&world, &epoch.system);
    // The same tools as the rig's toolbox, and the base the prompt has.
    {
        let mut records = store.records();
        if let cortex_agent::Record::Epoch(stored) = &mut records[0].1 {
            stored.tools.clone_from(&epoch.tools);
            stored.base = epoch.base;
            stored.instructions.clear();
        }
        let _ = records;
    }
    drop(rig);
    let rebuilt = MemorySessionStore::new();
    {
        let mut writer = rebuilt.clone();
        for (turn, mut record) in store.records() {
            if let cortex_agent::Record::Epoch(stored) = &mut record {
                stored.tools.clone_from(&epoch.tools);
                stored.base = epoch.base;
                stored.instructions.clear();
            }
            cortex_testkit::block_on(cortex_agent::SessionStore::append(
                &mut writer,
                &turn,
                record,
            ))
            .unwrap();
        }
    }
    let mut setup = Setup::new(&world);
    setup.id = "s_2".to_owned();
    setup.memory = rebuilt.clone();
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("after");
        rig.events.turns(1).await;
    });
    assert_eq!(
        rebuilt.epochs().len(),
        1,
        "no new epoch: the prompt is the same"
    );
    let request = last_request(&world.provider);
    assert!(
        has_reasoning(&request.messages),
        "the old reasoning is valid still"
    );
}

/// A client whose stable prefix is not the beginning of the one before
/// it: the loop's debug-build check must stop the turn.
struct Shifting {
    inner: ScriptedProvider,
    requests: std::sync::atomic::AtomicU64,
}

impl ModelClient for Shifting {
    fn stream(&self, request: ModelRequest, cancel: CancelToken) -> ModelStream {
        self.inner.stream(request, cancel)
    }

    fn stable_prefix(&self, _request: &ModelRequest) -> Option<Vec<u8>> {
        let count = self
            .requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(format!("{{\"request\":{count}").into_bytes())
    }
}

#[cfg(debug_assertions)]
#[test]
fn the_loops_check_stops_a_request_that_does_not_begin_with_the_one_before() {
    let world = World::new(
        "prefix_check_fires",
        [
            calls(&[("c1", "read", json!({"path": "a"}))]),
            says("Unreached."),
        ],
    );
    let mut setup = Setup::new(&world);
    setup.model = RoleModel::new(
        Arc::new(Shifting {
            inner: world.provider.clone(),
            requests: std::sync::atomic::AtomicU64::new(0),
        }),
        fixtures::model(),
    );
    let rig = setup.start(&world);
    rig.events
        .loop_panic_expected
        .store(true, std::sync::atomic::Ordering::SeqCst);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let end = &rig.events.ends()[0];
    let error = end.error.as_ref().unwrap();
    assert_eq!(error.code, "loop_panicked");
    assert!(
        error
            .message
            .contains("does not begin with the one before it"),
        "{}",
        error.message
    );
    assert_eq!(
        world.provider.requests().len(),
        1,
        "the second was never sent"
    );
}

#[test]
fn a_history_read_again_is_the_one_built_live() {
    // Tool rounds, notices, a compaction that replays recent turns, and a
    // change of model, each followed by a reload that changes nothing. The
    // loop's own check (a debug build's) holds each request after a reload
    // to the one before it, byte for byte, and so does this test.
    let world = World::new(
        "reload_equal",
        [
            Response::new()
                .reasoning("First.")
                .text("Reading.")
                .tool_call("c1", "read", &json!({"path": "a"}))
                .end(StopReason::ToolCalls),
            Response::new()
                .reasoning("Then.")
                .text("Read it.")
                .end(StopReason::End),
            says("Second answer."),
            says("A summary."),
            says("After the summary."),
            says("On the other model."),
            says("Last."),
        ],
    );
    let mut setup = Setup::new(&world);
    setup.context = ContextSettings {
        keep_recent_tokens: 20,
        ..ContextSettings::default()
    };
    let rig = setup.start(&world);
    rig.instructions
        .set(&root_file(&rig, "AGENTS.md"), "Use tabs.");
    let reload = || {
        let change = rig.handle.change_history().unwrap();
        drop(change);
    };
    let turn = |text: &str, count: usize| {
        world.run(async {
            rig.prompt(text);
            rig.events.turns(count).await;
        });
    };
    rig.handle
        .notice(NoticeSource::Plan, "The user added a task.");
    turn("first", 1);
    reload();
    rig.instructions
        .set(&root_file(&rig, "AGENTS.md"), "Use spaces.");
    turn("second", 2);
    reload();
    rig.handle.compact(None).unwrap();
    world.run(async {
        rig.events.turns(3).await;
    });
    reload();
    turn("third", 4);
    rig.handle.set_role_model(
        ModelRole::Main,
        RoleModel::new(Arc::new(world.provider.clone()), fixtures::plain_model()),
    );
    turn("fourth", 5);
    reload();
    turn("fifth", 6);

    assert!(
        rig.events
            .ends()
            .iter()
            .all(|end| end.outcome == TurnOutcome::Completed),
        "{:#?}",
        rig.events.ends()
    );
    let requests: Vec<ModelRequest> = world
        .provider
        .requests()
        .into_iter()
        .filter(|request| {
            request.role == ModelRole::Main
                && !all_text(request.messages.last().unwrap()).contains(SUMMARY_INSTRUCTIONS)
        })
        .collect();
    // Within each epoch, each request's messages begin with the request
    // before it's, a reload or not.
    for pair in requests.windows(2) {
        if pair[0].system == pair[1].system && pair[0].model.info.id == pair[1].model.info.id {
            let before = &pair[0].messages;
            if pair[1].messages.len() >= before.len()
                && pair[1].messages[..before.len()] == before[..]
            {
                continue;
            }
            // The one place the history may start again: the compaction.
            assert!(
                all_text(&pair[1].messages[0]).contains("A summary."),
                "a request after a reload does not begin with the one before it:\n{before:#?}\n{:#?}",
                pair[1].messages
            );
        }
    }
    assert_eq!(rig.store.summaries().len(), 1);
}

#[test]
fn an_epoch_whose_prompt_the_log_holds_masked_is_not_gone_on_with() {
    let world = World::new("resume_altered", [says("Now.")]);
    let probe = Setup::new(&world).start(&world);
    world.run(async {
        probe.prompt("probe");
        probe.events.turns(1).await;
    });
    let epoch = probe.store.epochs().remove(0);
    let rebuilt = MemorySessionStore::new();
    {
        let mut writer = rebuilt.clone();
        let mut masked = epoch.clone();
        masked.altered = true;
        let turn = TurnId::new("t_1");
        for record in [
            cortex_agent::Record::Epoch(masked),
            cortex_agent::Record::Item(Item::UserMessage(UserMessageItem {
                id: ItemId::new("i_1"),
                parts: vec![Part::text("before")],
                delivery: None,
                created_at: None,
            })),
        ] {
            cortex_testkit::block_on(cortex_agent::SessionStore::append(
                &mut writer,
                &turn,
                record,
            ))
            .unwrap();
        }
    }
    let mut setup = Setup::new(&world);
    setup.id = "s_2".to_owned();
    setup.memory = rebuilt.clone();
    let rig = setup.start(&world);
    world.run(async {
        rig.prompt("after");
        rig.events.turns(1).await;
    });
    let epochs = rebuilt.epochs();
    assert_eq!(epochs.len(), 2, "the same prompt, but the log's is masked");
    assert_eq!(epochs[1].reason, Some(EpochReason::Resume));
}
