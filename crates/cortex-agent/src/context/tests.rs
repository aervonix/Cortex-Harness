//! What a context makes of a session read from its store.

use cortex_base::ContentHash;
use cortex_protocol::item::{
    AssistantMessageItem, NoticeItem, NoticeSource, SummaryItem, ToolOutcome, ToolResultItem,
    UserMessageItem,
};
use cortex_protocol::message::{Origin, ReasoningPart, ToolCallPart};
use cortex_protocol::{CallId, ItemId, ModelId, ProviderId, RawJson, Role};
use serde_json::json;

use super::*;

fn user(id: &str, text: &str) -> Item {
    Item::UserMessage(UserMessageItem {
        id: ItemId::new(id),
        parts: vec![Part::text(text)],
        delivery: None,
        created_at: None,
    })
}

fn thinking_answer(id: &str, call: Option<&str>) -> Item {
    let mut parts = vec![
        Part::Reasoning(ReasoningPart {
            text: "thinking".to_owned(),
            raw: Some(RawJson::new(r#"{"type":"thinking","signature":"s"}"#)),
        }),
        Part::Text(cortex_protocol::message::TextPart {
            text: "Answer.".to_owned(),
            raw: Some(RawJson::new(r#"{"type":"text","text":"Answer."}"#)),
        }),
    ];
    if let Some(call) = call {
        parts.push(Part::ToolCall(ToolCallPart {
            id: CallId::new(call),
            name: "read".to_owned(),
            arguments: json!({"path": "a"}),
            raw: None,
        }));
    }
    Item::AssistantMessage(AssistantMessageItem {
        id: ItemId::new(id),
        parts,
        stop_reason: None,
        raw_stop: None,
        stop_detail: None,
        truncated: false,
        origin: Some(Origin {
            protocol: "cortex-test".to_owned(),
            provider: ProviderId::new("test"),
            model: ModelId::new("test-model"),
            account_scope: None,
        }),
        usage: None,
        error: None,
        created_at: None,
    })
}

fn result(id: &str, call: &str) -> Item {
    Item::ToolResult(ToolResultItem {
        id: ItemId::new(id),
        call_id: CallId::new(call),
        tool: "read".to_owned(),
        outcome: ToolOutcome::Success,
        content: vec![Part::text("content")],
        payload: None,
        artifacts: Vec::new(),
        duration_ms: None,
        created_at: None,
    })
}

fn epoch(system: &str) -> EpochStart {
    EpochStart {
        provider: ProviderId::new("test"),
        protocol: "cortex-test".to_owned(),
        model: ModelId::new("test-model"),
        effort: None,
        system: system.to_owned(),
        tools: Vec::new(),
        reason: Some(EpochReason::Start),
        base: None,
        instructions: vec![("AGENTS.md".to_owned(), ContentHash::of(b"Use tabs."))],
        altered: false,
    }
}

fn has_reasoning(message: &Message) -> bool {
    message
        .parts
        .iter()
        .any(|part| matches!(part, Part::Reasoning(_)))
}

fn context() -> Context {
    Context::new(ContextParts::new(), &[])
}

#[test]
fn reasoning_is_replayed_only_under_the_prefix_it_was_produced_under() {
    let mut context = context();
    context.load(&Opened {
        items: vec![
            user("i_1", "go"),
            thinking_answer("i_2", None),
            user("i_3", "again"),
            thinking_answer("i_4", None),
        ],
        epochs: vec![(0, epoch("one")), (2, epoch("two"))],
        ..Opened::default()
    });
    let messages = context.history.messages();
    assert_eq!(messages.len(), 4);
    assert!(
        !has_reasoning(&messages[1]),
        "produced under the first prompt"
    );
    assert!(messages[1].origin.is_none());
    assert!(
        has_reasoning(&messages[3]),
        "produced under the prompt in force"
    );
    assert_eq!(context.epoch_number, 2);
    assert!(context.unchecked);
}

#[test]
fn a_model_epoch_keeps_the_reasoning_before_it() {
    let mut context = context();
    let mut model = epoch("one");
    model.model = ModelId::new("test-plain");
    model.reason = Some(EpochReason::Model);
    context.load(&Opened {
        items: vec![user("i_1", "go"), thinking_answer("i_2", None)],
        epochs: vec![(0, epoch("one")), (2, model)],
        ..Opened::default()
    });
    assert!(has_reasoning(&context.history.messages()[1]));
}

#[test]
fn a_summary_begins_the_history_and_the_turns_it_did_not_cover_follow_without_reasoning() {
    let mut context = context();
    let summary = Item::Summary(SummaryItem {
        id: ItemId::new("i_7"),
        text: "Today's date is 2026-11-14 (UTC). The summary.".to_owned(),
        covers_through: Some(ItemId::new("i_3")),
        usage: None,
        created_at: None,
    });
    context.load(&Opened {
        items: vec![
            user("i_1", "go"),
            thinking_answer("i_2", Some("c1")),
            result("i_3", "c1"),
            // Taken after the summary began: replayed after it.
            thinking_answer("i_4", Some("c2")),
            result("i_5", "c2"),
            user("i_6", "and then"),
            summary,
            thinking_answer("i_8", None),
        ],
        epochs: vec![(0, epoch("one")), (7, epoch("two"))],
        ..Opened::default()
    });
    let roles: Vec<&Role> = context.history.messages().iter().map(|m| &m.role).collect();
    assert_eq!(
        roles,
        [
            &Role::User,
            &Role::Assistant,
            &Role::Tool,
            &Role::User,
            &Role::Assistant
        ]
    );
    let messages = context.history.messages();
    assert!(
        matches!(&messages[0].parts[0], Part::Text(text) if text.text.starts_with(NOTICE_LEAD))
    );
    assert!(!has_reasoning(&messages[1]), "a replayed turn has none");
    assert!(has_reasoning(&messages[4]), "the new epoch's own has");
    assert_eq!(
        context.last_user,
        Some(vec![Part::text("and then")]),
        "the last user message is known from before the summary"
    );
}

#[test]
fn the_instruction_files_the_model_has_are_read_back_from_the_branch() {
    let mut context = context();
    let notice = |id: &str, text: String| {
        Item::Notice(NoticeItem {
            id: ItemId::new(id),
            source: NoticeSource::Instructions,
            text,
            created_at: None,
        })
    };
    let place = |shown: &str| Place {
        path: AbsPath::parse("/w/x", cortex_base::PathStyle::Posix).unwrap(),
        shown: shown.to_owned(),
        user: false,
        dir: AbsPath::parse("/w", cortex_base::PathStyle::Posix).unwrap(),
    };
    let found = Found {
        place: place("src/AGENTS.md"),
        text: "Nested.".to_owned(),
        cut: false,
    };
    context.load(&Opened {
        items: vec![
            user("i_1", "go"),
            notice(
                "i_2",
                notice_text(
                    &Change::Nested {
                        found: found.clone(),
                        dir: "src".to_owned(),
                    },
                    DEFAULT_CAP,
                ),
            ),
            notice(
                "i_3",
                notice_text(&Change::Removed("AGENTS.md".to_owned()), DEFAULT_CAP),
            ),
        ],
        epochs: vec![(0, epoch("one"))],
        ..Opened::default()
    });
    assert_eq!(context.given.files.get("AGENTS.md"), Some(&None));
    assert_eq!(
        context.given.files.get("src/AGENTS.md"),
        Some(&Some(found.hash()))
    );
}

#[test]
fn a_compaction_that_kept_the_prompt_forgets_the_nested_files_given_before_it() {
    let mut context = context();
    let nested = Item::Notice(NoticeItem {
        id: ItemId::new("i_2"),
        source: NoticeSource::Instructions,
        text: notice_text(
            &Change::Nested {
                found: Found {
                    place: Place {
                        path: AbsPath::parse("/w/src/AGENTS.md", cortex_base::PathStyle::Posix)
                            .unwrap(),
                        shown: "src/AGENTS.md".to_owned(),
                        user: false,
                        dir: AbsPath::parse("/w/src", cortex_base::PathStyle::Posix).unwrap(),
                    },
                    text: "Nested.".to_owned(),
                    cut: false,
                },
                dir: "src".to_owned(),
            },
            DEFAULT_CAP,
        ),
        created_at: None,
    });
    let summary = Item::Summary(SummaryItem {
        id: ItemId::new("i_3"),
        text: "The summary.".to_owned(),
        covers_through: None,
        usage: None,
        created_at: None,
    });
    let mut compaction = epoch("one");
    compaction.reason = Some(EpochReason::Compaction);
    // The same prompt in both epochs: a compaction that kept it.
    context.load(&Opened {
        items: vec![user("i_1", "go"), nested, summary],
        epochs: vec![(0, epoch("one")), (3, compaction)],
        ..Opened::default()
    });
    assert!(!context.given.files.contains_key("src/AGENTS.md"));
    assert!(context.given.files.contains_key("AGENTS.md"));
}

#[test]
fn the_epochs_first_user_message_carries_its_own_date_live_and_read_again() {
    let dated = |id: &str, text: &str, at: &str| {
        Item::UserMessage(UserMessageItem {
            id: ItemId::new(id),
            parts: vec![Part::text(text)],
            delivery: None,
            created_at: Some(at.into()),
        })
    };
    let items = [
        dated("i_1", "go", "2026-11-12T09:00:00Z"),
        thinking_answer("i_2", None),
        dated("i_3", "again", "2026-11-13T09:00:00Z"),
    ];
    let mut live = context();
    for item in &items {
        live.push(item);
    }
    let mut read = context();
    read.load(&Opened {
        items: items.to_vec(),
        epochs: vec![(0, epoch("one"))],
        ..Opened::default()
    });
    read.epoch = None;
    assert_eq!(live.history.messages(), read.history.messages());
    let Part::Text(date) = &live.history.messages()[0].parts[0] else {
        panic!("a date part");
    };
    assert_eq!(date.text, "Today's date is 2026-11-12 (UTC).");
    assert_eq!(
        live.history.messages()[2].parts.len(),
        1,
        "only the first is dated"
    );
    assert_eq!(read.date.as_deref(), Some("2026-11-12"));
}

#[test]
fn turns_replayed_after_a_summary_are_the_same_live_and_read_again() {
    // A notice came before the first prompt, so the dated message is
    // among the turns the summary did not cover; and one answer held
    // only reasoning.
    let mut only_reasoning = thinking_answer("i_4", None);
    if let Item::AssistantMessage(message) = &mut only_reasoning {
        message.parts.truncate(1);
    }
    let items = vec![
        Item::Notice(NoticeItem {
            id: ItemId::new("i_1"),
            source: NoticeSource::Plan,
            text: "The plan.".to_owned(),
            created_at: None,
        }),
        Item::UserMessage(UserMessageItem {
            id: ItemId::new("i_2"),
            parts: vec![Part::text("go")],
            delivery: None,
            created_at: Some("2026-11-12T09:00:00Z".into()),
        }),
        thinking_answer("i_3", None),
        only_reasoning,
        user("i_5", "more"),
    ];
    let mut live = context();
    for item in &items {
        live.push(item);
    }
    // Live: the summary covers the notice alone, and the rest is replayed.
    let summary = Item::Summary(SummaryItem {
        id: ItemId::new("i_6"),
        text: "The summary.".to_owned(),
        covers_through: Some(ItemId::new("i_1")),
        usage: None,
        created_at: None,
    });
    let mut replayed = History::default();
    replayed.push(&summary);
    replayed.extend_projected(&live.history, 1);

    let mut read = context();
    let mut logged = items.clone();
    logged.push(summary);
    read.load(&Opened {
        items: logged,
        epochs: vec![(0, epoch("one"))],
        ..Opened::default()
    });
    assert_eq!(replayed.messages(), read.history.messages());
    assert_eq!(replayed.fingerprints(), read.history.fingerprints());
    assert!(
        !replayed
            .messages()
            .iter()
            .any(|message| message.parts.is_empty())
    );
}
