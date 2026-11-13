//! The assistant message being built from a response's events
//! (`docs/architecture/03-agent-loop.md`, "Streaming").
//!
//! A [`Draft`] takes the content events of one attempt and gives two
//! things back: the delta to show front ends, and, when a tool call's
//! block ends, the call with its arguments parsed. Arguments are untrusted
//! partial JSON until then, and are parsed once.

use std::collections::BTreeMap;

use cortex_llm::{BlockKind, ModelEvent};
use cortex_protocol::event::{Delta, TextDelta, ToolArgsDelta, ToolCallDelta};
use cortex_protocol::message::{ProviderBlockPart, ReasoningPart, TextPart, ToolCallPart};
use cortex_protocol::{CallId, Part, RawJson};
use cortex_tools::Json;

/// One block of a response, as far as it has arrived.
#[derive(Debug, Clone)]
enum Block {
    Text {
        text: String,
        raw: Option<RawJson>,
    },
    Reasoning {
        text: String,
        raw: Option<RawJson>,
    },
    ToolCall {
        id: CallId,
        name: String,
        /// The fragments so far.
        json: String,
        /// The arguments, once the block has ended: the value the model
        /// wrote, or its text as a JSON string when that was not JSON.
        arguments: Option<Json>,
        raw: Option<RawJson>,
    },
    Provider {
        raw: Option<RawJson>,
    },
}

/// A tool call whose block has ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EndedCall {
    /// The block's index in the response.
    pub(crate) index: u32,
    /// The provider's id for the call.
    pub(crate) id: CallId,
    /// The tool's name.
    pub(crate) name: String,
    /// The arguments, or why the model's text was not JSON.
    pub(crate) arguments: Result<Json, String>,
}

/// What an event added.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Applied {
    /// Nothing a front end is shown.
    Nothing,
    /// Streaming content.
    Delta(Delta),
    /// A tool call is complete.
    Call(EndedCall),
}

/// Whether the tool calls of a response are kept in its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Calls {
    /// The complete ones are kept: each will get a result.
    Keep,
    /// None is kept: the response failed, and none will run.
    Strip,
}

/// The content of one attempt at a response.
#[derive(Debug, Clone, Default)]
pub(crate) struct Draft {
    blocks: BTreeMap<u32, Block>,
}

impl Draft {
    /// Takes one content event. Events that are not content are ignored.
    pub(crate) fn apply(&mut self, event: ModelEvent) -> Applied {
        match event {
            ModelEvent::BlockStart { index, kind } => {
                self.blocks.entry(index).or_insert_with(|| match kind {
                    BlockKind::Text => Block::Text {
                        text: String::new(),
                        raw: None,
                    },
                    BlockKind::Reasoning => Block::Reasoning {
                        text: String::new(),
                        raw: None,
                    },
                    // Named by the `ToolCallStart` that follows.
                    BlockKind::ToolCall | BlockKind::ProviderBlock => Block::Provider { raw: None },
                });
                Applied::Nothing
            }
            ModelEvent::TextDelta { index, text } => {
                let block = self.blocks.entry(index).or_insert_with(|| Block::Text {
                    text: String::new(),
                    raw: None,
                });
                if let Block::Text { text: so_far, .. } = block {
                    so_far.push_str(&text);
                    Applied::Delta(Delta::Text(TextDelta { text }))
                } else {
                    Applied::Nothing
                }
            }
            ModelEvent::ReasoningDelta { index, text } => {
                let block = self.blocks.entry(index).or_insert_with(|| Block::Reasoning {
                    text: String::new(),
                    raw: None,
                });
                if let Block::Reasoning { text: so_far, .. } = block {
                    so_far.push_str(&text);
                    Applied::Delta(Delta::Reasoning(TextDelta { text }))
                } else {
                    Applied::Nothing
                }
            }
            ModelEvent::ToolCallStart { index, id, name } => {
                let call_id = CallId::new(id);
                self.blocks.insert(
                    index,
                    Block::ToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        json: String::new(),
                        arguments: None,
                        raw: None,
                    },
                );
                Applied::Delta(Delta::ToolCall(ToolCallDelta { call_id, name }))
            }
            ModelEvent::ToolArgsDelta { index, json } => match self.blocks.get_mut(&index) {
                Some(Block::ToolCall {
                    id,
                    json: so_far,
                    arguments: None,
                    ..
                }) => {
                    so_far.push_str(&json);
                    Applied::Delta(Delta::ToolArgs(ToolArgsDelta {
                        call_id: id.clone(),
                        json,
                    }))
                }
                _ => Applied::Nothing,
            },
            ModelEvent::BlockEnd { index, raw } => self.end_block(index, raw),
            _ => Applied::Nothing,
        }
    }

    fn end_block(&mut self, index: u32, block_raw: RawJson) -> Applied {
        match self.blocks.get_mut(&index) {
            Some(Block::Text { raw, .. } | Block::Reasoning { raw, .. } | Block::Provider { raw }) => {
                *raw = Some(block_raw);
                Applied::Nothing
            }
            Some(Block::ToolCall {
                id,
                name,
                json,
                arguments,
                raw,
            }) => {
                if arguments.is_some() {
                    // The block ended twice. The first end stands.
                    return Applied::Nothing;
                }
                *raw = Some(block_raw);
                let parsed = parse_arguments(json);
                *arguments = Some(match &parsed {
                    Ok(value) => value.clone(),
                    Err(_) => Json::String(std::mem::take(json)),
                });
                Applied::Call(EndedCall {
                    index,
                    id: id.clone(),
                    name: name.clone(),
                    arguments: parsed,
                })
            }
            None => {
                // A block that had no start and no content: the provider's
                // own JSON is all there is of it.
                self.blocks.insert(
                    index,
                    Block::Provider {
                        raw: Some(block_raw),
                    },
                );
                Applied::Nothing
            }
        }
    }

    /// Whether the last block is a tool call whose arguments never
    /// finished: what a response cut off by the output limit leaves.
    pub(crate) fn ends_in_unfinished_call(&self) -> bool {
        matches!(
            self.blocks.values().next_back(),
            Some(Block::ToolCall {
                arguments: None,
                ..
            })
        )
    }

    /// The message's parts, in the model's order. A tool call whose block
    /// never ended is left out: its arguments are incomplete and it is
    /// never run.
    pub(crate) fn parts(&self, calls: Calls) -> Vec<Part> {
        self.blocks
            .values()
            .filter_map(|block| match block {
                Block::Text { text, raw } => (!text.is_empty()).then(|| {
                    Part::Text(TextPart {
                        text: text.clone(),
                        raw: raw.clone(),
                    })
                }),
                Block::Reasoning { text, raw } => (!text.is_empty() || raw.is_some()).then(|| {
                    Part::Reasoning(ReasoningPart {
                        text: text.clone(),
                        raw: raw.clone(),
                    })
                }),
                Block::ToolCall {
                    id,
                    name,
                    arguments: Some(arguments),
                    raw,
                    ..
                } if calls == Calls::Keep => Some(Part::ToolCall(ToolCallPart {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                    raw: raw.clone(),
                })),
                Block::ToolCall { .. } => None,
                Block::Provider { raw } => raw.as_ref().map(|raw| {
                    Part::ProviderBlock(ProviderBlockPart {
                        block_type: block_type(raw),
                        raw: raw.clone(),
                    })
                }),
            })
            .collect()
    }
}

/// Parses a tool call's arguments, once, when its block has ended. A call
/// with no arguments at all is a call with none: an empty object.
fn parse_arguments(text: &str) -> Result<Json, String> {
    if text.trim().is_empty() {
        return Ok(Json::Object(serde_json::Map::new()));
    }
    serde_json::from_str(text).map_err(|error| error.to_string())
}

/// The provider's name for a block's type, where its JSON has one.
fn block_type(raw: &RawJson) -> Option<String> {
    let value: Json = serde_json::from_str(raw.as_str()).ok()?;
    value.get("type")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn raw(value: &Json) -> RawJson {
        RawJson::new(value.to_string())
    }

    fn text(index: u32, piece: &str) -> ModelEvent {
        ModelEvent::TextDelta {
            index,
            text: piece.to_owned(),
        }
    }

    fn call_events(index: u32, id: &str, fragments: &[&str]) -> Vec<ModelEvent> {
        let mut events = vec![
            ModelEvent::BlockStart {
                index,
                kind: BlockKind::ToolCall,
            },
            ModelEvent::ToolCallStart {
                index,
                id: id.to_owned(),
                name: "read".to_owned(),
            },
        ];
        events.extend(fragments.iter().map(|fragment| ModelEvent::ToolArgsDelta {
            index,
            json: (*fragment).to_owned(),
        }));
        events
    }

    #[test]
    fn blocks_become_parts_in_index_order_whatever_order_they_arrive_in() {
        let mut draft = Draft::default();
        assert_eq!(
            draft.apply(text(2, "second")),
            Applied::Delta(Delta::Text(TextDelta {
                text: "second".to_owned()
            }))
        );
        draft.apply(ModelEvent::BlockStart {
            index: 0,
            kind: BlockKind::Reasoning,
        });
        draft.apply(ModelEvent::ReasoningDelta {
            index: 0,
            text: "why".to_owned(),
        });
        draft.apply(ModelEvent::BlockEnd {
            index: 0,
            raw: raw(&json!({"type": "thinking", "signature": "s"})),
        });
        draft.apply(text(2, " half"));
        draft.apply(ModelEvent::BlockEnd {
            index: 5,
            raw: raw(&json!({"type": "server_tool_use", "id": "x"})),
        });

        let parts = draft.parts(Calls::Keep);
        let [Part::Reasoning(reasoning), Part::Text(said), Part::ProviderBlock(block)] =
            parts.as_slice()
        else {
            panic!("{parts:?}");
        };
        assert_eq!(reasoning.text, "why");
        assert!(reasoning.raw.is_some(), "the opaque payload is kept");
        assert_eq!(said.text, "second half");
        assert_eq!(said.raw, None, "the block has not ended");
        assert_eq!(block.block_type.as_deref(), Some("server_tool_use"));
    }

    #[test]
    fn arguments_are_parsed_once_when_the_block_ends() {
        let mut draft = Draft::default();
        for event in call_events(1, "c1", &["{\"pa", "th\":\"a.rs\"}"]) {
            assert!(!matches!(draft.apply(event), Applied::Call(_)));
        }
        assert!(draft.ends_in_unfinished_call());
        assert_eq!(draft.parts(Calls::Keep), [], "an unfinished call is no part");

        let ended = draft.apply(ModelEvent::BlockEnd {
            index: 1,
            raw: raw(&json!({"type": "tool_use"})),
        });
        let Applied::Call(call) = ended else {
            panic!("{ended:?}");
        };
        assert_eq!((call.index, call.id.as_str(), call.name.as_str()), (1, "c1", "read"));
        assert_eq!(call.arguments, Ok(json!({"path": "a.rs"})));
        assert!(!draft.ends_in_unfinished_call());

        // A second end and late fragments change nothing.
        assert_eq!(
            draft.apply(ModelEvent::BlockEnd {
                index: 1,
                raw: raw(&json!({})),
            }),
            Applied::Nothing
        );
        assert_eq!(
            draft.apply(ModelEvent::ToolArgsDelta {
                index: 1,
                json: "x".to_owned(),
            }),
            Applied::Nothing
        );
        let parts = draft.parts(Calls::Keep);
        let [Part::ToolCall(part)] = parts.as_slice() else {
            panic!("{parts:?}");
        };
        assert_eq!(part.arguments, json!({"path": "a.rs"}));
        assert_eq!(draft.parts(Calls::Strip), [], "a failed response keeps no call");
    }

    #[test]
    fn text_that_is_not_json_is_kept_as_a_string_and_reported() {
        let mut draft = Draft::default();
        for event in call_events(0, "c1", &["{\"path\": "]) {
            draft.apply(event);
        }
        let Applied::Call(call) = draft.apply(ModelEvent::BlockEnd {
            index: 0,
            raw: raw(&json!({})),
        }) else {
            panic!("the block ended");
        };
        assert!(call.arguments.is_err());
        let parts = draft.parts(Calls::Keep);
        let [Part::ToolCall(part)] = parts.as_slice() else {
            panic!("{parts:?}");
        };
        assert_eq!(part.arguments, json!("{\"path\": "));
    }

    #[test]
    fn a_call_with_no_arguments_has_an_empty_object() {
        let mut draft = Draft::default();
        for event in call_events(0, "c1", &[]) {
            draft.apply(event);
        }
        let Applied::Call(call) = draft.apply(ModelEvent::BlockEnd {
            index: 0,
            raw: raw(&json!({})),
        }) else {
            panic!("the block ended");
        };
        assert_eq!(call.arguments, Ok(json!({})));
    }

    #[test]
    fn fragments_for_a_block_that_is_not_a_call_are_ignored() {
        let mut draft = Draft::default();
        draft.apply(text(0, "words"));
        assert_eq!(
            draft.apply(ModelEvent::ToolArgsDelta {
                index: 0,
                json: "{}".to_owned(),
            }),
            Applied::Nothing
        );
        assert_eq!(
            draft.apply(ModelEvent::ReasoningDelta {
                index: 0,
                text: "x".to_owned(),
            }),
            Applied::Nothing
        );
        assert_eq!(draft.apply(ModelEvent::Usage(cortex_llm::Usage::default())), Applied::Nothing);
        assert_eq!(draft.parts(Calls::Keep).len(), 1);
    }
}
