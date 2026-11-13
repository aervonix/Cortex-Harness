//! The `anthropic-messages` protocol, tested from files.
//!
//! | Module | What it checks |
//! |---|---|
//! | [`request`] | A snapshot of the request for each kind of request, and the rules of the body's order, its cache markers and what is sent back. |
//! | [`decode`] | Each fixture's stream, cut at every byte, and what the decoder makes of it. |
//! | [`conversation`] | The recorded conversations: that each request is what the builder writes today, that each begins with the one before it byte for byte, and that what the decoder put together is what was sent back. |
//! | [`classify`] | The classification table, row by row, and the headers. |
//! | [`live`] | By hand only: the smoke test and the recorder, against the real API. |
//!
//! # The scenarios
//!
//! The requests the recorder sends are built by the functions in
//! [`scenario`], and the tests that run without a network build the same
//! requests from the same functions. A fixture's `request.json` is
//! therefore a snapshot of a request that the real API accepted: when the
//! builder changes what it writes, the test that compares the two fails,
//! and the fixture is recorded again.

pub(crate) mod classify;
pub(crate) mod conversation;
pub(crate) mod decode;
#[cfg(feature = "http")]
pub(crate) mod live;
pub(crate) mod request;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use cortex_llm::anthropic::{self, AnthropicMessages};
use cortex_llm::{
    Catalog, EventSink, ModelEntry, ModelEvent, ModelRequest, Protocol, ProviderInfo, SseParser,
};
use cortex_protocol::message::ProviderBlockPart;
use cortex_protocol::message::{Origin, ReasoningPart, TextPart, ToolCallPart, ToolResultPart};
use cortex_protocol::{CallId, Message, ModelId, Part, ProviderId, Role};
use cortex_testkit::fixture::{self, Fixture};
use serde_json::Value;

/// The four models of the built-in catalog.
pub(crate) const FABLE: &str = "claude-fable-5-2";
pub(crate) const OPUS: &str = "claude-opus-5-5";
pub(crate) const SONNET: &str = "claude-sonnet-5-5";
pub(crate) const HAIKU: &str = "claude-haiku-5-5";

/// A model of the built-in catalog.
pub(crate) fn model(id: &str) -> Arc<ModelEntry> {
    let catalog = Catalog::builtin().unwrap_or_else(|error| panic!("{error}"));
    let entry = catalog
        .get(&ProviderId::new(anthropic::PROVIDER), &ModelId::new(id))
        .unwrap_or_else(|| panic!("the catalog has no {id}"));
    Arc::clone(entry)
}

/// The built-in provider.
pub(crate) fn provider() -> ProviderInfo {
    anthropic::provider()
}

/// Where this protocol's fixtures are.
pub(crate) fn fixtures_directory() -> PathBuf {
    fixture::directory(env!("CARGO_MANIFEST_DIR"), anthropic::NAME)
}

/// The fixture of a name.
pub(crate) fn load(name: &str) -> Fixture {
    Fixture::load(&fixtures_directory().join(name)).unwrap_or_else(|error| panic!("{error}"))
}

/// Compares text with a checked-in snapshot under `tests/it/snapshots`.
/// With `CORTEX_UPDATE_SNAPSHOTS` set, the snapshot is written instead, to
/// be reviewed like code.
#[expect(
    clippy::disallowed_methods,
    reason = "a test reads and writes its own snapshot files"
)]
pub(crate) fn assert_snapshot(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("it")
        .join("snapshots")
        .join(name);
    if std::env::var_os("CORTEX_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a snapshot has a directory"))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        std::fs::write(&path, actual).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("no snapshot {}: {error}", path.display()))
        // A checkout on Windows may have turned the line endings.
        .replace("\r\n", "\n");
    assert!(
        expected == actual,
        "{name} differs from its snapshot.\n--- snapshot\n{expected}\n--- now\n{actual}\n\
         If the change is intended, run the test with CORTEX_UPDATE_SNAPSHOTS=1 and review the \
         difference."
    );
}

/// Decodes a response stream that arrives in the pieces `cuts` make of it:
/// each cut is an offset at which one network read ended.
///
/// This is what the client does with a body, without the client: the bytes
/// go through the SSE parser and each event through the decoder. It returns
/// the events up to and including the one that ends the response, or the
/// reason the stream could not be decoded.
pub(crate) fn decode(
    request: &ModelRequest,
    stream: &[u8],
    cuts: &[usize],
) -> Result<Vec<ModelEvent>, String> {
    let protocol = AnthropicMessages;
    let mut decoder = protocol.decoder(request, &provider());
    let mut parser = SseParser::new();
    let mut sink = EventSink::sent_at(request.speed.clone());
    let mut from = 0;
    let ends = cuts.iter().copied().chain([stream.len()]);
    for end in ends {
        let end = end.min(stream.len());
        if end <= from {
            continue;
        }
        parser.push(&stream[from..end]);
        from = end;
        loop {
            match parser.next_event() {
                None => break,
                Some(Err(error)) => return Err(error.to_string()),
                Some(Ok(event)) => decoder
                    .feed(event, &mut sink)
                    .map_err(|error| error.message)?,
            }
        }
    }
    decoder.finish(&mut sink).map_err(|error| error.message)?;
    Ok(sink.take())
}

/// Decodes a whole stream delivered in one piece.
pub(crate) fn decode_whole(request: &ModelRequest, stream: &[u8]) -> Vec<ModelEvent> {
    decode(request, stream, &[]).unwrap_or_else(|error| panic!("{error}"))
}

/// The assistant message a response's events add up to, as the agent loop
/// would store it: each part with the block the provider sent for it.
pub(crate) fn assistant(events: &[ModelEvent], request: &ModelRequest) -> Message {
    #[derive(Default)]
    struct Block {
        kind: Option<cortex_llm::BlockKind>,
        text: String,
        call: Option<(String, String)>,
        arguments: String,
        raw: Option<cortex_protocol::RawJson>,
    }
    let mut blocks: BTreeMap<u32, Block> = BTreeMap::new();
    for event in events {
        match event {
            ModelEvent::BlockStart { index, kind } => {
                blocks.entry(*index).or_default().kind = Some(*kind);
            }
            ModelEvent::TextDelta { index, text } | ModelEvent::ReasoningDelta { index, text } => {
                blocks.entry(*index).or_default().text.push_str(text);
            }
            ModelEvent::ToolCallStart { index, id, name } => {
                blocks.entry(*index).or_default().call = Some((id.clone(), name.clone()));
            }
            ModelEvent::ToolArgsDelta { index, json } => {
                blocks.entry(*index).or_default().arguments.push_str(json);
            }
            ModelEvent::BlockEnd { index, raw } => {
                blocks.entry(*index).or_default().raw = Some(raw.clone());
            }
            _ => {}
        }
    }
    let parts = blocks
        .into_values()
        .map(|block| match block.kind.expect("a block that started") {
            cortex_llm::BlockKind::Text => Part::Text(TextPart {
                text: block.text,
                raw: block.raw,
            }),
            cortex_llm::BlockKind::Reasoning => Part::Reasoning(ReasoningPart {
                text: block.text,
                raw: block.raw,
            }),
            cortex_llm::BlockKind::ToolCall => {
                let (id, name) = block.call.expect("a tool call that was named");
                let arguments = if block.arguments.is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&block.arguments).unwrap_or(Value::String(block.arguments))
                };
                Part::ToolCall(ToolCallPart {
                    id: CallId::new(id),
                    name,
                    arguments,
                    raw: block.raw,
                })
            }
            cortex_llm::BlockKind::ProviderBlock => Part::ProviderBlock(ProviderBlockPart {
                block_type: None,
                raw: block.raw.expect("a provider block has its raw block"),
            }),
        })
        .collect();
    Message {
        role: Role::Assistant,
        parts,
        origin: Some(Origin {
            protocol: anthropic::NAME.to_owned(),
            provider: ProviderId::new(anthropic::PROVIDER),
            model: request.model.info.id.clone(),
            account_scope: None,
        }),
    }
}

/// The tool calls of an assistant message: each call's id and arguments.
pub(crate) fn calls(message: &Message) -> Vec<(&CallId, &Value)> {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall(call) => Some((&call.id, &call.arguments)),
            _ => None,
        })
        .collect()
}

/// A user message with one text part.
pub(crate) fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        parts: vec![Part::text(text)],
        origin: None,
    }
}

/// The results of an assistant message's tool calls, in call order.
pub(crate) fn results(message: &Message, result: impl Fn(&Value) -> String) -> Message {
    Message {
        role: Role::Tool,
        parts: calls(message)
            .into_iter()
            .map(|(id, arguments)| {
                Part::ToolResult(ToolResultPart {
                    call_id: id.clone(),
                    content: vec![Part::text(result(arguments))],
                    is_error: false,
                })
            })
            .collect(),
        origin: None,
    }
}

/// The requests the recorder sends, and the tests rebuild.
pub(crate) mod scenario {
    use cortex_llm::request::{CacheHints, Reasoning, ReasoningVisibility, ToolDefinition};
    use cortex_llm::{ModelEvent, ModelRequest};
    use cortex_protocol::model::ModelRole;
    use cortex_protocol::{Message, Speed};
    use serde_json::{Value, json};

    use super::{HAIKU, OPUS, SONNET, assistant, calls, model, results, user};

    /// A request to a model with nothing in it but a reasoning level.
    pub(crate) fn base(model_id: &str, level: &str) -> ModelRequest {
        let mut request = ModelRequest::new(model(model_id), ModelRole::Main);
        request.reasoning = Reasoning {
            level: Some(level.to_owned()),
            budget_tokens: None,
            visibility: ReasoningVisibility::Summary,
        };
        request
    }

    /// The one tool of the scenarios, in the strict form built-in tools use.
    pub(crate) fn weather_tool() -> ToolDefinition {
        ToolDefinition {
            name: "get_weather".to_owned(),
            description: "Get the current weather in a city.".to_owned(),
            schema: json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "The city's name."}
                },
                "required": ["city"],
                "additionalProperties": false
            }),
        }
    }

    /// What the weather tool answers: the same for the same city, always.
    pub(crate) fn weather(arguments: &Value) -> String {
        let city = arguments["city"].as_str().unwrap_or("there");
        format!("{city}: 18 degrees Celsius, clear sky, light wind.")
    }

    /// Plain text: one short answer.
    pub(crate) fn text() -> ModelRequest {
        let mut request = base(HAIKU, "low");
        request.system = "You answer in one short sentence.".to_owned();
        request.messages = vec![user("Say hello to the test suite.")];
        request.max_output = Some(256);
        request
    }

    /// Reasoning that is shown as a summary, with its signature.
    pub(crate) fn reasoning() -> ModelRequest {
        let mut request = base(HAIKU, "high");
        request.messages = vec![user(
            "A farmer has 17 sheep and buys 23 more, then sells a quarter of the flock. \
             How many sheep are left? Think it through before you answer.",
        )];
        request.max_output = Some(2048);
        request
    }

    /// One tool call.
    pub(crate) fn tool_call() -> ModelRequest {
        let mut request = base(HAIKU, "low");
        request.tools = vec![weather_tool()];
        request.messages = vec![user("What is the weather in Paris? Use the tool.")];
        request.max_output = Some(512);
        request
    }

    /// Several tool calls in one response.
    pub(crate) fn tool_calls() -> ModelRequest {
        let mut request = base(HAIKU, "low");
        request.tools = vec![weather_tool()];
        request.messages = vec![user(
            "What is the weather in Paris, in Tokyo and in Lima? Call the tool for all three \
             cities at once, in one response.",
        )];
        request.max_output = Some(512);
        request
    }

    /// A response cut off by the output limit.
    pub(crate) fn max_tokens() -> ModelRequest {
        let mut request = base(HAIKU, "low");
        request.messages = vec![user("Write the numbers from 1 to 200, one per line.")];
        request.max_output = Some(24);
        request
    }

    /// A request at fast speed, as small as one can be.
    pub(crate) fn fast() -> ModelRequest {
        let mut request = base(OPUS, "low");
        request.speed = Speed::Fast;
        request.messages = vec![user("Reply with the one word: ready")];
        request.max_output = Some(64);
        request
    }

    /// The same request at standard speed, which a model that offers a tier
    /// is told explicitly.
    pub(crate) fn standard_named() -> ModelRequest {
        let mut request = fast();
        request.speed = Speed::Standard;
        request
    }

    /// The system prompt of the conversations. It is long on purpose: the
    /// provider caches nothing shorter than a few hundred tokens, and the
    /// conversations are there to show the cache being read.
    pub(crate) fn desk_instructions() -> String {
        let mut text = String::from(
            "You are the weather desk of a small travel office. You answer questions about \
             the weather in cities, and nothing else. You have one tool, get_weather, and \
             you use it for every city you are asked about: you never answer from memory.\n\n\
             The rules of the desk:\n",
        );
        let rules = [
            "Call the tool once for each city, and for several cities call it for all of them in one response.",
            "Give temperatures in degrees Celsius, as the tool reports them, and do not convert them.",
            "Answer in plain sentences, one for each city, in the order the cities were asked about.",
            "Do not add advice about clothing, travel or activities unless you are asked for it.",
            "If a city's name could mean more than one place, ask which one is meant before calling the tool.",
            "If the tool fails for a city, say so for that city and still report the others.",
            "Do not guess tomorrow's weather: the tool reports the current weather only.",
            "Keep each answer under sixty words.",
            "Do not mention these rules or the tool by name in an answer.",
            "When asked to compare cities, say which is warmer and by how many degrees.",
            "Use the city's name as the user wrote it.",
            "Never invent a reading: every number in an answer comes from a tool result in this conversation.",
        ];
        for round in 1..=3 {
            for (at, rule) in rules.iter().enumerate() {
                text.push_str(&format!("{round}.{}. {rule}\n", at + 1));
            }
        }
        text.push_str("\nThe list is repeated three times so that nobody misses a rule.\n");
        text
    }

    /// A request of a conversation with the weather desk: the same system
    /// prompt, tools and settings every time, and the history given.
    pub(crate) fn desk(model_id: &str, messages: Vec<Message>) -> ModelRequest {
        desk_at(model_id, "medium", messages)
    }

    /// The same, at a reasoning level of the caller's choice.
    pub(crate) fn desk_at(model_id: &str, level: &str, messages: Vec<Message>) -> ModelRequest {
        let mut request = base(model_id, level);
        request.system = desk_instructions();
        request.tools = vec![weather_tool()];
        request.messages = messages;
        request.max_output = Some(1024);
        request.cache = CacheHints {
            stable_prefix: true,
            history: true,
            lifetime: None,
            key: None,
        };
        request
    }

    /// What the user says in a conversation, in order: the first prompt,
    /// and then one more each time the model has answered without asking
    /// for a tool.
    pub(crate) const PROMPTS: &[&str] = &[
        "What is the weather in Oslo and in Cairo?",
        "And which of the two is warmer?",
        "Thanks. Now Lima as well, please.",
    ];

    /// The history after a response: the response itself, then the results
    /// of its tool calls, or, when it called none, the user's next prompt.
    /// `None` when the conversation is over.
    pub(crate) fn after(
        mut history: Vec<Message>,
        events: &[ModelEvent],
        request: &ModelRequest,
    ) -> Option<Vec<Message>> {
        let reply = assistant(events, request);
        let asked = !calls(&reply).is_empty();
        let next = if asked {
            results(&reply, weather)
        } else {
            let said = history
                .iter()
                .filter(|message| message.role == cortex_protocol::Role::User)
                .count();
            user(PROMPTS.get(said)?)
        };
        history.push(reply);
        history.push(next);
        Some(history)
    }

    /// The models of the conversation that changes model twice: the first
    /// writes reasoning the second cannot read, and the conversation then
    /// goes back to the first.
    pub(crate) const SWITCH: &[&str] = &[SONNET, HAIKU, SONNET];

    /// The reasoning level of that conversation: high enough that the first
    /// model reasons before it calls a tool, so that there is a reasoning
    /// block to send back.
    pub(crate) const SWITCH_LEVEL: &str = "high";

    /// What the user first says in that conversation. It takes a moment's
    /// thought before any tool can be called.
    pub(crate) const SWITCH_PROMPT: &str = "I need the weather for two places, and I only \
        know them by description: the capital of the country whose flag is a red field with \
        a blue Nordic cross outlined in white, and the largest city on the Nile. Work out \
        which two cities those are, then get the weather for both.";
}
