//! What is sent to the model: the seam for context assembly
//! (`docs/architecture/07-sessions-and-context.md`).
//!
//! The loop keeps no history of its own. It tells a [`Context`] of every
//! item it records, and before each request asks it for the request's
//! content: the system prompt, the tool definitions and the messages. Two
//! of the loop's states are the context's to trigger and to do:
//! `Preparing` goes to `Compacting` when the context says it is over its
//! budget, and `Compacting` is one call to [`Context::compact`].
//!
//! The build step for context management supplies the real one: the system
//! prompt, instruction files, context epochs, budgeting and compaction.
//! Until then [`StubContext`] sends everything, never asks to compact, and
//! cannot.

use cortex_base::{CancelToken, Classified, ErrorKind};
use cortex_llm::{CacheHints, ToolDefinition};
use cortex_protocol::message::ToolResultPart;
use cortex_protocol::{Item, Message, Part, Role};
use cortex_tools::{BoxFuture, ToolSpec};

/// The content of one request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Assembled {
    /// The system prompt.
    pub system: String,
    /// The tools the model may call, in the order they are declared.
    pub tools: Vec<ToolDefinition>,
    /// The conversation, oldest first.
    pub messages: Vec<Message>,
    /// Where the cacheable parts end.
    pub cache: CacheHints,
}

/// The context could not be made to fit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ContextError {
    /// Why, for a person.
    pub message: String,
}

impl ContextError {
    /// An error with this message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Classified for ContextError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::ContextOverflow
    }
}

/// What a session sends its model, and how it is kept within the model's
/// context window.
pub trait Context: Send {
    /// Replaces what the context holds with the items of a session as it
    /// was opened.
    fn load(&mut self, items: &[Item]);

    /// An item was recorded. Items arrive in the order the log has them: an
    /// assistant message, then one result for each of its tool calls, in
    /// call order.
    fn push(&mut self, item: &Item);

    /// The content of the next request.
    fn assemble(&self) -> Assembled;

    /// Whether the next request needs a compaction first.
    fn needs_compaction(&self) -> bool;

    /// Summarises and starts a new context epoch. Called between tool
    /// rounds, never inside one. It stops promptly when `cancel` fires.
    fn compact<'a>(
        &'a mut self,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<(), ContextError>>;
}

/// The stand-in context: a fixed system prompt, the tools as they were when
/// the session started, and every item ever recorded.
#[derive(Debug, Clone, Default)]
pub struct StubContext {
    system: String,
    tools: Vec<ToolDefinition>,
    messages: Vec<Message>,
    cache_key: Option<String>,
}

impl StubContext {
    /// A context with this system prompt and these tools, and no history.
    #[must_use]
    pub fn new(system: impl Into<String>, tools: &[&ToolSpec]) -> Self {
        Self {
            system: system.into(),
            tools: tools.iter().map(|spec| definition(spec)).collect(),
            messages: Vec::new(),
            cache_key: None,
        }
    }

    /// Sets the key that stays the same for the session, for providers that
    /// route requests with one key to one cache.
    #[must_use]
    pub fn with_cache_key(mut self, key: impl Into<String>) -> Self {
        self.cache_key = Some(key.into());
        self
    }
}

impl Context for StubContext {
    fn load(&mut self, items: &[Item]) {
        self.messages.clear();
        for item in items {
            append(&mut self.messages, item);
        }
    }

    fn push(&mut self, item: &Item) {
        append(&mut self.messages, item);
    }

    fn assemble(&self) -> Assembled {
        Assembled {
            system: self.system.clone(),
            tools: self.tools.clone(),
            messages: self.messages.clone(),
            cache: CacheHints {
                stable_prefix: true,
                history: true,
                lifetime: None,
                key: self.cache_key.clone(),
            },
        }
    }

    fn needs_compaction(&self) -> bool {
        false
    }

    fn compact<'a>(
        &'a mut self,
        _cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<(), ContextError>> {
        Box::pin(std::future::ready(Err(ContextError::new(
            "The conversation no longer fits the model's context window, and this build cannot \
             compact it yet. Start a new session to continue.",
        ))))
    }
}

/// The definition of a tool as a request declares it.
#[must_use]
pub fn definition(spec: &ToolSpec) -> ToolDefinition {
    ToolDefinition {
        name: spec.name.clone(),
        description: spec.description.clone(),
        schema: spec.parameters.clone(),
    }
}

/// Adds one recorded item to a conversation, in the one neutral shape every
/// protocol is given: an assistant message with its calls, then one `tool`
/// message that holds the results in call order.
///
/// An assistant message with no content is left out: a response that was
/// interrupted or failed before it said anything is in the log, and no
/// provider accepts an empty message.
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
            parts: vec![Part::text(notice.text.clone())],
            origin: None,
        }),
        Item::Summary(summary) => messages.push(Message {
            role: Role::User,
            parts: vec![Part::text(summary.text.clone())],
            origin: None,
        }),
        // A kind of item a later version records: nothing this build can
        // send.
        Item::Unknown(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::item::{
        AssistantMessageItem, NoticeItem, NoticeSource, ToolOutcome, ToolResultItem,
        UserMessageItem,
    };
    use cortex_protocol::message::ToolCallPart;
    use cortex_protocol::{CallId, ItemId};
    use serde_json::json;

    use super::*;

    fn user(text: &str) -> Item {
        Item::UserMessage(UserMessageItem {
            id: ItemId::new("i_1"),
            parts: vec![Part::text(text)],
            delivery: None,
            created_at: None,
        })
    }

    fn assistant(parts: Vec<Part>) -> Item {
        Item::AssistantMessage(AssistantMessageItem {
            id: ItemId::new("i_2"),
            parts,
            stop_reason: None,
            raw_stop: None,
            stop_detail: None,
            truncated: false,
            origin: None,
            usage: None,
            error: None,
            created_at: None,
        })
    }

    fn call(id: &str) -> Part {
        Part::ToolCall(ToolCallPart {
            id: CallId::new(id),
            name: "read".to_owned(),
            arguments: json!({"path": "a"}),
            raw: None,
        })
    }

    fn result(id: &str, outcome: ToolOutcome) -> Item {
        Item::ToolResult(ToolResultItem {
            id: ItemId::new("i_3"),
            call_id: CallId::new(id),
            tool: "read".to_owned(),
            outcome,
            content: vec![Part::text("content")],
            payload: None,
            artifacts: Vec::new(),
            duration_ms: None,
            created_at: None,
        })
    }

    #[test]
    fn results_of_one_round_share_one_message_in_call_order() {
        let mut context = StubContext::new("You are a test.", &[]).with_cache_key("s_1");
        context.push(&user("go"));
        context.push(&assistant(vec![
            Part::text("Reading."),
            call("c1"),
            call("c2"),
        ]));
        context.push(&result("c1", ToolOutcome::Success));
        context.push(&result("c2", ToolOutcome::Denied));
        context.push(&Item::Notice(NoticeItem {
            id: ItemId::new("i_9"),
            source: NoticeSource::Plan,
            text: "The plan changed.".to_owned(),
            created_at: None,
        }));

        let assembled = context.assemble();
        assert_eq!(assembled.system, "You are a test.");
        assert_eq!(assembled.cache.key.as_deref(), Some("s_1"));
        let roles: Vec<&Role> = assembled.messages.iter().map(|m| &m.role).collect();
        assert_eq!(
            roles,
            [&Role::User, &Role::Assistant, &Role::Tool, &Role::User]
        );
        let results = &assembled.messages[2].parts;
        let flags: Vec<(&str, bool)> = results
            .iter()
            .map(|part| match part {
                Part::ToolResult(result) => (result.call_id.as_str(), result.is_error),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(flags, [("c1", false), ("c2", true)]);
        assert!(!context.needs_compaction());
    }

    #[test]
    fn an_assistant_message_with_nothing_in_it_is_not_sent() {
        let mut context = StubContext::default();
        context.load(&[user("go"), assistant(Vec::new()), user("again")]);
        let roles: Vec<Role> = context
            .assemble()
            .messages
            .into_iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(roles, [Role::User, Role::User]);

        context.load(&[user("fresh")]);
        assert_eq!(context.assemble().messages.len(), 1, "load replaces");
    }

    #[test]
    fn the_stub_cannot_compact_and_says_so() {
        let mut context = StubContext::default();
        let cancel = CancelToken::new();
        let mut future = context.compact(&cancel);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(outcome) = future.as_mut().poll(&mut cx) else {
            panic!("the stub never waits");
        };
        let error = outcome.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ContextOverflow);
        assert_eq!(error.code(), "context_overflow");
    }
}
