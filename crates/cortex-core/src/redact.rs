//! Secrets and the session log (`docs/security.md`, section 4;
//! `docs/architecture/07-sessions-and-context.md`, "Secrets in sessions").
//!
//! The filter itself is `cortex-base`'s [`SecretFilter`]. Here it is fitted
//! to the session store's hook, and the two rules the hook cannot apply by
//! itself are applied:
//!
//! **A provider's own block that holds a secret is never written.** The
//! store's hook does not see `raw` blocks, because a changed byte makes the
//! provider refuse the block on replay. So before an assistant message is
//! stored, each of its raw blocks is looked at ([`for_disk`]). If the
//! filter finds a secret in one, the message is stored as its neutral
//! projection: text and tool calls without their raw blocks, reasoning
//! with its visible text only, provider blocks left out, and no origin, so
//! that no protocol ever takes it for something it can replay raw. The
//! session that is running keeps the message as it arrived and replays it
//! as before; only the disk gets the projection, marked as altered.
//!
//! **After a resume, reasoning that was tied to changed history is not
//! replayed** ([`resumed`]). An entry the filter changed, in its text or as
//! above, is marked in the log. On the providers that tie each reasoning
//! block to everything before it, a block written after such an entry no
//! longer matches the history a resumed session would send, and the
//! provider refuses the request. So from the first marked entry to the end
//! of its context epoch, every assistant message is sent as its neutral
//! projection, as a message from another provider would be.

use std::borrow::Cow;

use cortex_base::SecretFilter;
use cortex_protocol::Item;
use cortex_protocol::Part;
use cortex_protocol::item::AssistantMessageItem;
use cortex_session::{Body, Entry, Redactor};
use serde_json::Value;

/// The core's filter, as the session store's hook. It is read each time:
/// a key the core finds while the session runs is masked from then on.
#[derive(Debug, Clone)]
pub(crate) struct LogRedactor(pub(crate) crate::provider::SharedFilter);

impl Redactor for LogRedactor {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .redact(text)
    }
}

/// Whether the filter finds a secret in a provider's block.
///
/// The block is JSON text, so what it says is written with escapes: a key
/// that follows a line break stands after `\n`, and one may be spelled
/// with `\u` escapes all through. The text is looked at as it is, and
/// then each of its strings as it reads once the escapes are undone.
fn raw_holds_secret(filter: &SecretFilter, raw: &str) -> bool {
    if filter.finds(raw) {
        return true;
    }
    if !raw.contains('\\') {
        return false;
    }
    fn any(filter: &SecretFilter, value: &Value) -> bool {
        match value {
            Value::String(text) => filter.finds(text),
            Value::Array(items) => items.iter().any(|item| any(filter, item)),
            Value::Object(fields) => fields
                .iter()
                .any(|(key, item)| filter.finds(key) || any(filter, item)),
            Value::Null | Value::Bool(_) | Value::Number(_) => false,
        }
    }
    serde_json::from_str::<Value>(raw).is_ok_and(|value| any(filter, &value))
}

/// Whether the filter finds a secret in one of the message's raw blocks.
fn has_secret_in_raw(filter: &SecretFilter, message: &AssistantMessageItem) -> bool {
    let in_usage = message
        .usage
        .as_ref()
        .and_then(|usage| usage.raw.as_ref())
        .is_some_and(|raw| raw_holds_secret(filter, raw.as_str()));
    in_usage
        || message.parts.iter().any(|part| {
            let raw = match part {
                Part::Text(text) => text.raw.as_ref(),
                Part::ToolCall(call) => call.raw.as_ref(),
                Part::Reasoning(reasoning) => reasoning.raw.as_ref(),
                Part::ProviderBlock(block) => Some(&block.raw),
                _ => None,
            };
            raw.is_some_and(|raw| raw_holds_secret(filter, raw.as_str()))
        })
}

/// The message as a protocol that did not write it is sent it: text and
/// tool calls kept, reasoning without its opaque payload, provider blocks
/// left out, and nothing that says who produced it.
pub(crate) fn neutral(message: &AssistantMessageItem) -> AssistantMessageItem {
    let mut neutral = message.clone();
    neutral.parts = message.parts.iter().filter_map(neutral_part).collect();
    neutral.origin = None;
    if let Some(usage) = &mut neutral.usage {
        usage.raw = None;
    }
    neutral
}

/// One part without the provider's own block: the part's neutral fields,
/// or nothing when the block was all there was to it.
pub(crate) fn neutral_part(part: &Part) -> Option<Part> {
    match part {
        Part::Text(text) => {
            let mut text = text.clone();
            text.raw = None;
            Some(Part::Text(text))
        }
        Part::ToolCall(call) => {
            let mut call = call.clone();
            call.raw = None;
            Some(Part::ToolCall(call))
        }
        Part::Reasoning(reasoning) => {
            let mut reasoning = reasoning.clone();
            reasoning.raw = None;
            // With no payload and nothing to show there is nothing.
            (!reasoning.text.is_empty()).then_some(Part::Reasoning(reasoning))
        }
        Part::ProviderBlock(_) => None,
        other => Some(other.clone()),
    }
}

/// Whether the message has anything a protocol would replay raw.
fn is_replayable(message: &AssistantMessageItem) -> bool {
    message.origin.is_some()
        && message.parts.iter().any(|part| match part {
            Part::Text(text) => text.raw.is_some(),
            Part::ToolCall(call) => call.raw.is_some(),
            Part::Reasoning(reasoning) => reasoning.raw.is_some(),
            Part::ProviderBlock(_) => true,
            _ => false,
        })
}

/// What `item` is stored as, when that is not the item itself: an
/// assistant message with a secret in one of its raw blocks, as its
/// neutral projection. `None` for every other item, which is stored as it
/// is (its text still passes the store's hook).
pub(crate) fn for_disk(filter: &SecretFilter, item: &Item) -> Option<Item> {
    match item {
        Item::AssistantMessage(message) if has_secret_in_raw(filter, message) => {
            Some(Item::AssistantMessage(neutral(message)))
        }
        _ => None,
    }
}

/// The items of a resumed session's active branch, as they may be sent.
#[derive(Debug, Default)]
pub(crate) struct Resumed {
    /// The items, in order.
    pub(crate) items: Vec<Item>,
    /// How many assistant messages lost what would have been replayed raw,
    /// because the log before them is not what the model was sent.
    pub(crate) projected: usize,
}

/// Reads the items out of a branch's entries. From the first item entry
/// that is marked as altered to the end of its context epoch, an assistant
/// message is given as its neutral projection.
///
/// - **Only an item counts.** A record the model is never sent (`plan`,
///   `lease`, `outcome`, `base`) can be marked when the filter masked
///   something in it, and changes nothing the model was sent.
/// - **The mark ends where the reasoning after it is tied to another
///   history:** at a `summary` entry, after which nothing of the earlier
///   history is replayed with its reasoning, and at an `epoch` entry that
///   was not a change of model. An epoch begun by a change of model keeps
///   the history and its prefix, so the mark goes on through it.
pub(crate) fn resumed(entries: &[Entry]) -> Resumed {
    let mut resumed = Resumed::default();
    let mut altered = false;
    for entry in entries {
        let ends = match &entry.body {
            Body::Summary { .. } => true,
            Body::Epoch(epoch) => epoch.reason.as_deref() != Some("model"),
            _ => false,
        };
        if ends {
            altered = false;
        }
        if entry.body.as_item().is_some() {
            altered |= entry.redacted;
        }
        let Some(item) = entry.body.as_item() else {
            continue;
        };
        match item {
            Item::AssistantMessage(message) if altered && is_replayable(&message) => {
                resumed.projected += 1;
                resumed
                    .items
                    .push(Item::AssistantMessage(neutral(&message)));
            }
            item => resumed.items.push(item),
        }
    }
    resumed
}

#[cfg(test)]
mod tests {
    use cortex_base::Secret;
    use cortex_protocol::ids::{ItemId, RawJson};
    use cortex_protocol::message::{
        Origin, ProviderBlockPart, ReasoningPart, TextPart, ToolCallPart,
    };
    use cortex_protocol::{CallId, ModelId, ProviderId};
    use serde_json::json;

    use super::*;

    const KEY: &str = "cortex-raw-test-key-55aa";

    fn filter() -> SecretFilter {
        SecretFilter::new().with_value(&Secret::new(KEY))
    }

    fn message(reasoning_raw: &str) -> AssistantMessageItem {
        AssistantMessageItem {
            id: ItemId::new("i_2"),
            parts: vec![
                Part::Reasoning(ReasoningPart {
                    text: "thinking it over".to_owned(),
                    raw: Some(RawJson::new(reasoning_raw)),
                }),
                Part::Reasoning(ReasoningPart {
                    text: String::new(),
                    raw: Some(RawJson::new(
                        r#"{"type":"redacted_thinking","data":"AAAA"}"#,
                    )),
                }),
                Part::Text(TextPart {
                    text: "Running it.".to_owned(),
                    raw: Some(RawJson::new(r#"{"type":"text","text":"Running it."}"#)),
                }),
                Part::ToolCall(ToolCallPart {
                    id: CallId::new("c1"),
                    name: "shell".to_owned(),
                    arguments: json!({"command": "ls"}),
                    raw: Some(RawJson::new(r#"{"type":"tool_use","id":"c1"}"#)),
                }),
                Part::ProviderBlock(ProviderBlockPart {
                    block_type: Some("server_tool_use".to_owned()),
                    raw: RawJson::new(r#"{"type":"server_tool_use"}"#),
                }),
            ],
            stop_reason: Some(cortex_protocol::StopReason::ToolCalls),
            raw_stop: None,
            stop_detail: None,
            truncated: false,
            origin: Some(Origin {
                protocol: "anthropic-messages".to_owned(),
                provider: ProviderId::new("anthropic"),
                model: ModelId::new("claude-opus-5-5"),
                account_scope: None,
            }),
            usage: None,
            error: None,
            created_at: None,
        }
    }

    /// A provider's block is JSON text. A key in it stands after a written
    /// out line break, or is written with escapes all through, and is a key
    /// all the same.
    #[test]
    fn a_secret_behind_the_escapes_of_a_raw_block_is_found() {
        // In a known format, so that only its shape gives it away.
        const TOKEN: &str = "ghp_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8";
        let known = filter();
        let unknown = SecretFilter::new();
        let after_break = serde_json::to_string(
            &json!({"type": "thinking", "thinking": format!("Found in .env:\n{TOKEN}\tand more")}),
        )
        .unwrap();
        assert!(after_break.contains("\\nghp_"), "{after_break}");
        // Every character of the harness's own key as a `\u` escape.
        let spelled: String = KEY
            .chars()
            .map(|c| format!("\\u{:04x}", c as u32))
            .collect();
        let escaped_value = format!("{{\"type\":\"thinking\",\"thinking\":\"{spelled}\"}}");
        assert!(!escaped_value.contains(KEY));
        // And one as an object's key.
        let as_a_key = format!("{{\"note\\n{TOKEN}\":1}}");
        for (filter, raw) in [
            (&unknown, after_break.as_str()),
            (&known, escaped_value.as_str()),
            (&unknown, as_a_key.as_str()),
        ] {
            let stored = for_disk(filter, &Item::AssistantMessage(message(raw)));
            assert!(stored.is_some(), "kept as it was: {raw}");
        }
        // A block with escapes and no secret is kept.
        let clean = r#"{"type":"thinking","thinking":"line one\nline two \u00e9"}"#;
        assert!(for_disk(&known, &Item::AssistantMessage(message(clean))).is_none());
    }

    #[test]
    fn a_message_with_a_secret_in_a_raw_block_is_stored_as_its_neutral_projection() {
        let filter = filter();
        let clean = message(r#"{"type":"thinking","thinking":"x","signature":"sig"}"#);
        assert!(for_disk(&filter, &Item::AssistantMessage(clean)).is_none());

        let leaking = message(&format!(
            r#"{{"type":"thinking","thinking":"the key is {KEY}","signature":"sig"}}"#
        ));
        let Some(Item::AssistantMessage(stored)) =
            for_disk(&filter, &Item::AssistantMessage(leaking))
        else {
            panic!("it was not altered");
        };
        // Nothing raw is left, anywhere, and nothing says who wrote it.
        let text = serde_json::to_string(&stored).unwrap();
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains("\"raw\""), "{text}");
        assert!(stored.origin.is_none());
        // Text and tool calls carry over; reasoning keeps what it showed;
        // the reasoning with nothing to show and the provider block go.
        let kinds: Vec<&str> = stored
            .parts
            .iter()
            .map(|part| match part {
                Part::Text(_) => "text",
                Part::ToolCall(_) => "tool_call",
                Part::Reasoning(_) => "reasoning",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["reasoning", "text", "tool_call"]);
        assert!(!is_replayable(&stored));
    }

    #[test]
    fn a_secret_in_any_kind_of_raw_block_counts() {
        let filter = filter();
        let leak = RawJson::new(format!(r#"{{"x":"{KEY}"}}"#));
        for index in 0..5 {
            let mut message = message(r#"{"type":"thinking"}"#);
            match &mut message.parts[index] {
                Part::Text(part) => part.raw = Some(leak.clone()),
                Part::ToolCall(part) => part.raw = Some(leak.clone()),
                Part::Reasoning(part) => part.raw = Some(leak.clone()),
                Part::ProviderBlock(part) => part.raw = leak.clone(),
                _ => unreachable!(),
            }
            assert!(
                for_disk(&filter, &Item::AssistantMessage(message)).is_some(),
                "part {index}"
            );
        }
        // A key in a well-known format counts as the harness's own does.
        let message = message(r#"{"thinking":"ghp_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8"}"#);
        assert!(for_disk(&filter, &Item::AssistantMessage(message)).is_some());
    }

    mod resumed_marks {
        use cortex_protocol::Timestamp;
        use cortex_protocol::item::{SummaryItem, UserMessageItem};
        use cortex_session::EntryId;
        use cortex_session::entry::{Epoch, PlanRecord};

        use super::*;

        fn entry(number: u32, body: Body, redacted: bool) -> Entry {
            Entry {
                id: EntryId::new(format!("e_{number:04}")),
                parent: None,
                ts: Timestamp::from("2026-11-14T00:00:00Z"),
                turn: None,
                redacted,
                body,
            }
        }

        fn user(number: u32, redacted: bool) -> Entry {
            entry(
                number,
                Body::User(UserMessageItem {
                    id: ItemId::new(format!("i_{number}")),
                    parts: vec![Part::text("go")],
                    delivery: None,
                    created_at: None,
                }),
                redacted,
            )
        }

        fn answer(number: u32) -> Entry {
            let mut item = message(r#"{"type":"thinking","signature":"s"}"#);
            item.id = ItemId::new(format!("i_{number}"));
            entry(
                number,
                Body::Assistant {
                    item,
                    request_id: None,
                },
                false,
            )
        }

        fn epoch(number: u32, reason: &str) -> Entry {
            entry(
                number,
                Body::Epoch(Epoch {
                    provider: ProviderId::new("anthropic"),
                    protocol: "anthropic-messages".to_owned(),
                    model: ModelId::new("claude-opus-5-5"),
                    effort: None,
                    system: "s".to_owned(),
                    tools: Vec::new(),
                    hash: Epoch::hash_of("s", &[]),
                    reason: Some(reason.to_owned()),
                    base: None,
                    instructions: Vec::new(),
                }),
                false,
            )
        }

        /// Which assistant messages keep what is replayed raw.
        fn raw(entries: &[Entry]) -> Vec<bool> {
            resumed(entries)
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::AssistantMessage(message) => Some(message.origin.is_some()),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn a_summary_ends_the_mark_and_a_mark_after_it_begins_again() {
            let summary = entry(
                4,
                Body::Summary {
                    item: SummaryItem {
                        id: ItemId::new("i_4"),
                        text: "The summary.".to_owned(),
                        covers_through: None,
                        usage: None,
                        created_at: None,
                    },
                    carried: Value::Null,
                },
                false,
            );
            let entries = [
                user(1, true),
                answer(2),
                answer(3),
                summary,
                answer(5),
                user(6, true),
                answer(7),
            ];
            assert_eq!(raw(&entries), [false, false, true, false]);
            assert_eq!(resumed(&entries).projected, 3);
        }

        #[test]
        fn only_an_item_marks() {
            let plan = entry(
                1,
                Body::Plan(PlanRecord {
                    plan: cortex_protocol::Plan::default(),
                }),
                true,
            );
            assert_eq!(raw(&[plan, user(2, false), answer(3)]), [true]);
        }

        #[test]
        fn a_change_of_model_carries_the_mark_and_another_epoch_ends_it() {
            assert_eq!(
                raw(&[user(1, true), epoch(2, "model"), answer(3)]),
                [false]
            );
            for reason in ["start", "resume", "compaction"] {
                assert_eq!(
                    raw(&[user(1, true), epoch(2, reason), answer(3)]),
                    [true],
                    "{reason}"
                );
            }
        }
    }
}
