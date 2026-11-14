//! The messages of a context epoch, as the next request sends them.
//!
//! The history only grows. Each recorded item is folded into the end of it
//! ([`append`]), and nothing already in it is ever changed: that is what
//! keeps a request's prefix the beginning of the next one's
//! (`docs/architecture/07-sessions-and-context.md`, "Why append-only").
//!
//! Beside the messages it keeps, for each one, the id of the last item in
//! it, its size and what kind of thing it is (for the budget and the
//! context record), and a fingerprint (for where a request first differs
//! from the one before it).

use std::hash::{DefaultHasher, Hasher};

use cortex_protocol::message::ToolResultPart;
use cortex_protocol::{Item, ItemId, Message, Part, Role};

use super::instructions::date_text;

/// What every notice and every summary the model is sent begins with.
///
/// A notice arrives in the user's role (decision 0034), so it says plainly
/// that it is the harness speaking. The text is part of the cached prefix:
/// it never changes.
pub const NOTICE_LEAD: &str = "Notice from the harness (Cortex-Harness), not from the user.\n\n";

/// The size an image is counted at, in bytes of text: about what a
/// provider charges for a picture of a screen, at the default ratio.
const IMAGE_BYTES: u64 = 4_000;

/// What a message of the history holds, for the context record's estimates
/// by source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The summary that begins a compacted epoch.
    Summary,
    /// What the user wrote.
    User,
    /// What the model answered.
    Assistant,
    /// The results of a round of tool calls.
    ToolResults,
    /// A notice from the harness.
    Notice,
}

/// One message of the history, with what is kept beside it.
#[derive(Debug, Clone)]
struct Meta {
    /// The id of the last item folded into the message.
    end: Option<ItemId>,
    source: Source,
    bytes: u64,
    fingerprint: u64,
}

/// The messages of an epoch.
#[derive(Debug, Clone, Default)]
pub(crate) struct History {
    messages: Vec<Message>,
    meta: Vec<Meta>,
    /// Whether the epoch's first message carries the date: its first user
    /// message does, or its summary.
    dated: bool,
}

impl History {
    /// The messages, oldest first.
    pub(crate) fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// How many messages there are.
    pub(crate) fn len(&self) -> usize {
        self.messages.len()
    }

    /// The fingerprint of each message, in order.
    pub(crate) fn fingerprints(&self) -> Vec<u64> {
        self.meta.iter().map(|meta| meta.fingerprint).collect()
    }

    /// The id of the last item in the first `count` messages.
    pub(crate) fn end_of(&self, count: usize) -> Option<ItemId> {
        self.meta[..count.min(self.meta.len())]
            .iter()
            .rev()
            .find_map(|meta| meta.end.clone())
    }

    /// The size in bytes of the messages from `from` on, by source.
    pub(crate) fn bytes_by_source(&self, from: usize) -> [(Source, u64); 5] {
        let mut out = [
            (Source::Summary, 0),
            (Source::User, 0),
            (Source::Assistant, 0),
            (Source::ToolResults, 0),
            (Source::Notice, 0),
        ];
        for meta in self.meta.iter().skip(from) {
            if let Some(slot) = out.iter_mut().find(|(source, _)| *source == meta.source) {
                slot.1 += meta.bytes;
            }
        }
        out
    }

    /// The size in bytes of every message.
    pub(crate) fn bytes(&self) -> u64 {
        self.meta.iter().map(|meta| meta.bytes).sum()
    }

    /// The size in bytes of the messages from `from` on.
    pub(crate) fn bytes_from(&self, from: usize) -> u64 {
        self.meta.iter().skip(from).map(|meta| meta.bytes).sum()
    }

    /// Adds one recorded item at the end.
    ///
    /// **The epoch's first user message carries the date** it was recorded
    /// on, as a text part before what the user wrote, when the epoch does
    /// not begin with a summary (which says the date itself). The date is
    /// the item's own, so the message is the same bytes whenever it is
    /// built, a resume's included; the stored item is not changed.
    pub(crate) fn push(&mut self, item: &Item) {
        let before = self.messages.len();
        match item {
            Item::Summary(_) => self.dated = true,
            Item::UserMessage(user) if !self.dated => {
                self.dated = true;
                if let Some(date) = user
                    .created_at
                    .as_ref()
                    .and_then(|at| at.as_str().get(..10))
                {
                    let mut dated = user.clone();
                    dated.parts.insert(0, Part::text(date_text(date)));
                    return self.push_shown(item, &Item::UserMessage(dated), before);
                }
            }
            _ => {}
        }
        self.push_shown(item, item, before);
    }

    /// Adds `shown` as the message of the recorded `item`.
    fn push_shown(&mut self, item: &Item, shown: &Item, before: usize) {
        append(&mut self.messages, shown);
        let source = match item {
            Item::UserMessage(_) => Source::User,
            Item::AssistantMessage(_) => Source::Assistant,
            Item::ToolResult(_) => Source::ToolResults,
            Item::Summary(_) => Source::Summary,
            Item::Notice(_) | Item::Unknown(_) => Source::Notice,
        };
        let id = item.id().map(ItemId::new);
        if self.messages.len() > before {
            let message = self.messages.last().map(measure).unwrap_or_default();
            self.meta.push(Meta {
                end: id,
                source,
                bytes: message.0,
                fingerprint: message.1,
            });
        } else if self.messages.len() == before
            && matches!(item, Item::ToolResult(_))
            && let (Some(last), Some(meta)) = (self.messages.last(), self.meta.last_mut())
        {
            // A result joined the results message of its round.
            let (bytes, fingerprint) = measure(last);
            meta.bytes = bytes;
            meta.fingerprint = fingerprint;
            meta.end = id;
        }
    }

    /// Drops the reasoning and the provider's own blocks from every
    /// assistant message: the history goes on into an epoch whose prefix is
    /// not the one it was produced under. Nothing of it has been sent in
    /// that epoch yet.
    pub(crate) fn project_all(&mut self) {
        for (message, meta) in self.messages.iter_mut().zip(&mut self.meta) {
            if message.role == Role::Assistant {
                *message = project(message);
                let (bytes, fingerprint) = measure(message);
                meta.bytes = bytes;
                meta.fingerprint = fingerprint;
            }
        }
    }

    /// Adds the messages of `other` from `from` on, without the reasoning
    /// or the provider's own blocks of its assistant messages: the turns a
    /// summary did not cover, replayed after it.
    pub(crate) fn extend_projected(&mut self, other: &Self, from: usize) {
        for (message, meta) in other.messages.iter().zip(&other.meta).skip(from) {
            let message = if message.role == Role::Assistant {
                project(message)
            } else {
                message.clone()
            };
            let (bytes, fingerprint) = measure(&message);
            self.messages.push(message);
            self.meta.push(Meta {
                end: meta.end.clone(),
                source: meta.source,
                bytes,
                fingerprint,
            });
        }
        self.dated = true;
    }

    /// Adds a recorded item at the end without the reasoning or the
    /// provider's own blocks of an assistant message: for a turn replayed
    /// in an epoch whose prefix is not the one it was produced under.
    pub(crate) fn push_projected(&mut self, item: &Item) {
        match item {
            Item::AssistantMessage(message) => {
                let mut projected = message.clone();
                projected.parts = message.parts.iter().filter_map(project_part).collect();
                projected.origin = None;
                self.push(&Item::AssistantMessage(projected));
            }
            other => self.push(other),
        }
    }
}

/// A message's size in bytes, as the budget counts it, and its
/// fingerprint.
fn measure(message: &Message) -> (u64, u64) {
    let mut bytes = 0u64;
    for part in &message.parts {
        bytes += part_bytes(part);
    }
    let mut hasher = DefaultHasher::new();
    hasher.write(message.role.as_str().as_bytes());
    hasher.write(
        serde_json::to_string(message)
            .unwrap_or_default()
            .as_bytes(),
    );
    (bytes, hasher.finish())
}

fn part_bytes(part: &Part) -> u64 {
    let len = |text: &str| u64::try_from(text.len()).unwrap_or(u64::MAX);
    match part {
        Part::Text(text) => len(&text.text),
        Part::Image(_) => IMAGE_BYTES,
        Part::ToolCall(call) => len(&call.name) + len(&call.arguments.to_string()),
        Part::ToolResult(result) => result.content.iter().map(part_bytes).sum(),
        Part::Reasoning(reasoning) => len(&reasoning.text),
        Part::ProviderBlock(block) => len(block.raw.as_str()),
        _ => 0,
    }
}

/// One part of an assistant message without the provider's own block: its
/// neutral fields, or nothing when it had only the block (reasoning,
/// provider blocks).
fn project_part(part: &Part) -> Option<Part> {
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
        Part::Reasoning(_) | Part::ProviderBlock(_) => None,
        other => Some(other.clone()),
    }
}

/// An assistant message as it is replayed in an epoch whose prefix is not
/// the one it was produced under: text and tool calls, without reasoning,
/// without the provider's own blocks, and without an origin, so that no
/// protocol takes it for something to send back raw.
#[must_use]
pub fn project(message: &Message) -> Message {
    Message {
        role: message.role.clone(),
        parts: message.parts.iter().filter_map(project_part).collect(),
        origin: None,
    }
}

/// Adds one recorded item to a conversation, in the one neutral shape every
/// protocol is given: an assistant message with its calls, then one `tool`
/// message that holds the results in call order.
///
/// An assistant message with no content is left out: a response that was
/// interrupted or failed before it said anything is in the log, and no
/// provider accepts an empty message. A notice and a summary are sent in
/// the user's role, each beginning with [`NOTICE_LEAD`].
pub fn append(messages: &mut Vec<Message>, item: &Item) {
    match item {
        Item::UserMessage(user) => messages.push(Message {
            role: Role::User,
            parts: user.parts.clone(),
            origin: None,
        }),
        Item::AssistantMessage(assistant) => {
            if !assistant.parts.is_empty() {
                messages.push(Message {
                    role: Role::Assistant,
                    parts: assistant.parts.clone(),
                    origin: assistant.origin.clone(),
                });
            }
        }
        Item::ToolResult(result) => {
            let part = Part::ToolResult(ToolResultPart {
                call_id: result.call_id.clone(),
                content: result.content.clone(),
                is_error: result.outcome != cortex_protocol::item::ToolOutcome::Success,
            });
            match messages.last_mut() {
                Some(last) if last.role == Role::Tool => last.parts.push(part),
                _ => messages.push(Message {
                    role: Role::Tool,
                    parts: vec![part],
                    origin: None,
                }),
            }
        }
        Item::Notice(notice) => messages.push(Message {
            role: Role::User,
            parts: vec![Part::text(format!("{NOTICE_LEAD}{}", notice.text))],
            origin: None,
        }),
        Item::Summary(summary) => messages.push(Message {
            role: Role::User,
            parts: vec![Part::text(format!("{NOTICE_LEAD}{}", summary.text))],
            origin: None,
        }),
        // A kind of item a later version records: nothing this build can
        // send.
        Item::Unknown(_) => {}
    }
}
