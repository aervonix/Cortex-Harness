//! The `openai-chat` protocol, tested from files.
//!
//! | Module | What it checks |
//! |---|---|
//! | [`request`] | A snapshot of the request for each kind of request, and the rules of the body's order, of what is sent back and of each flag. |
//! | [`decode`] | Each fixture's stream, cut at every byte, and what the decoder makes of it. |
//! | [`conversation`] | The recorded conversations: that each request is what the builder writes today, that each begins with the one before it byte for byte, and the switch from an Anthropic model to a compatible one and back. |
//! | [`classify`] | The classification table, row by row, the headers, and the recorded failures. |
//! | [`endpoints`] | The usage endpoints and the context probes, from the answers the servers document, and against a scripted server. |
//! | [`live`] | By hand only: the smoke tests and the recorder, against the real servers. |
//!
//! # The servers
//!
//! A fixture's name begins with the server it is from. Three are recorded
//! from the real API ([`DEEPSEEK`], [`OPENROUTER`], [`VENICE`]). The others
//! (OpenAI itself, and the local servers) are written by hand from each
//! server's documented format, and each fixture's note says so.
//!
//! # The scenarios
//!
//! The requests the recorder sends are built by the functions in
//! [`scenario`], and the tests that run without a network build the same
//! requests from the same functions. A recorded fixture's `request.json`
//! is therefore a snapshot of a request that a real server accepted: when
//! the builder changes what it writes, the test that compares the two
//! fails, and the fixture is recorded again.

pub(crate) mod classify;
pub(crate) mod conversation;
pub(crate) mod decode;
pub(crate) mod endpoints;
#[cfg(feature = "http")]
pub(crate) mod live;
pub(crate) mod request;

use std::path::PathBuf;
use std::sync::Arc;

use cortex_llm::openai_chat::{self, OpenAiChat, providers};
use cortex_llm::{
    Catalog, EventSink, ModelEntry, ModelEvent, ModelRequest, Protocol, ProviderInfo, SseParser,
};
use cortex_protocol::message::Origin;
use cortex_protocol::{Message, ModelId, ProviderId};
use cortex_testkit::fixture::{self, Fixture};

/// A server the scenarios are run against: a provider, the model that is
/// asked, and that model's names for little reasoning, a lot, and none.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Server {
    /// The provider's name, which a fixture's name begins with.
    pub(crate) provider: &'static str,
    /// The model the scenarios ask.
    pub(crate) model: &'static str,
    /// The model's level for little reasoning.
    pub(crate) low: &'static str,
    /// The model's level for a lot of it.
    pub(crate) high: &'static str,
    /// The model's level for none, where it has one.
    pub(crate) off: Option<&'static str>,
}

/// DeepSeek's own API.
pub(crate) const DEEPSEEK: Server = Server {
    provider: providers::DEEPSEEK,
    model: "deepseek-flash",
    low: "low",
    high: "high",
    off: Some("none"),
};

/// OpenRouter, with a model that always reasons.
pub(crate) const OPENROUTER: Server = Server {
    provider: providers::OPENROUTER,
    model: "z-ai/glm-5.3-flash",
    low: "low",
    high: "high",
    off: None,
};

/// Venice.ai.
pub(crate) const VENICE: Server = Server {
    provider: providers::VENICE,
    model: "deepseek-v4-flash-0731",
    low: "low",
    high: "high",
    off: Some("none"),
};

/// The three servers the fixtures are recorded from.
pub(crate) const RECORDED: &[Server] = &[DEEPSEEK, OPENROUTER, VENICE];

/// OpenRouter's entry for a model of OpenAI's, which has service tiers.
pub(crate) const OPENROUTER_LUNA: &str = "openai/gpt-6-luna";
/// OpenRouter's entry for a model of Anthropic's, which caches only where
/// a request marks it and signs its reasoning.
pub(crate) const OPENROUTER_CLAUDE: &str = "anthropic/claude-haiku-5.5";
/// OpenAI's own entry.
pub(crate) const OPENAI_LUNA: &str = "gpt-6-luna";

/// A model of the built-in catalog.
pub(crate) fn model(provider: &str, id: &str) -> Arc<ModelEntry> {
    let catalog = Catalog::builtin().unwrap_or_else(|error| panic!("{error}"));
    let entry = catalog
        .get(&ProviderId::new(provider), &ModelId::new(id))
        .unwrap_or_else(|| panic!("the catalog has no {provider}/{id}"));
    Arc::clone(entry)
}

/// A model nobody's catalog has: what a user's own entry for a local model
/// comes to. `extra` adds or replaces keys of the entry, as JSON.
pub(crate) fn local_model(provider: &str, id: &str, extra: &str) -> Arc<ModelEntry> {
    let mut entry = serde_json::json!({
        "provider": provider, "id": id, "name": id, "protocol": openai_chat::NAME,
        "context_window": 131_072, "max_output": 16_384,
    });
    let extra: serde_json::Value = serde_json::from_str(extra).expect("the extra keys are JSON");
    for (key, value) in extra.as_object().expect("the extra keys are an object") {
        entry[key] = value.clone();
    }
    let file = serde_json::json!({"version": "test", "model": [entry]});
    let catalog = Catalog::from_json(&file.to_string()).unwrap_or_else(|error| panic!("{error}"));
    Arc::clone(&catalog.models()[0])
}

/// A built-in provider.
pub(crate) fn provider(name: &str) -> ProviderInfo {
    providers::builtin(name)
        .unwrap_or_else(|| panic!("there is no built-in provider `{name}`"))
        .info
}

/// The provider a fixture is from: the beginning of its name.
pub(crate) fn provider_of(fixture: &str) -> ProviderInfo {
    provider(fixture.split('-').next().unwrap_or_default())
}

/// Where this protocol's fixtures are.
pub(crate) fn fixtures_directory() -> PathBuf {
    fixture::directory(env!("CARGO_MANIFEST_DIR"), openai_chat::NAME)
}

/// The fixture of a name.
pub(crate) fn load(name: &str) -> Fixture {
    Fixture::load(&fixtures_directory().join(name)).unwrap_or_else(|error| panic!("{error}"))
}

/// The names of every fixture on disk, sorted.
#[expect(
    clippy::disallowed_methods,
    reason = "a test lists its own fixture directory"
)]
pub(crate) fn fixture_names() -> Vec<String> {
    let entries = std::fs::read_dir(fixtures_directory()).expect("the fixtures directory");
    let mut names: Vec<String> = entries
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .into_string()
                .expect("a name")
        })
        .collect();
    names.sort();
    names
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
    provider: &ProviderInfo,
    stream: &[u8],
    cuts: &[usize],
) -> Result<Vec<ModelEvent>, String> {
    let mut decoder = OpenAiChat.decoder(request, provider);
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
pub(crate) fn decode_whole(
    request: &ModelRequest,
    provider: &ProviderInfo,
    stream: &[u8],
) -> Vec<ModelEvent> {
    decode(request, provider, stream, &[]).unwrap_or_else(|error| panic!("{error}"))
}

/// Decodes a stream written in a test: each item is the data of one event.
pub(crate) fn decode_events(
    request: &ModelRequest,
    provider: &ProviderInfo,
    events: &[&str],
) -> Result<Vec<ModelEvent>, String> {
    let stream: String = events
        .iter()
        .map(|data| format!("data: {data}\n\n"))
        .collect();
    decode(request, provider, stream.as_bytes(), &[])
}

/// The assistant message a response's events add up to, as the agent loop
/// would store it: each part with the block the decoder stored for it, and
/// this protocol and provider as where it came from.
pub(crate) fn assistant(
    events: &[ModelEvent],
    request: &ModelRequest,
    provider: &ProviderInfo,
) -> Message {
    let mut message = crate::anthropic::assistant(events, request);
    message.origin = Some(Origin {
        protocol: openai_chat::NAME.to_owned(),
        provider: provider.id.clone(),
        model: request.model.info.id.clone(),
        account_scope: None,
    });
    message
}

/// The requests the recorder sends, and the tests rebuild.
pub(crate) mod scenario {
    use cortex_llm::request::{CacheHints, Reasoning, ReasoningVisibility};
    use cortex_llm::{ModelEvent, ModelRequest, ProviderInfo};
    use cortex_protocol::model::ModelRole;
    use cortex_protocol::{Message, Speed};

    use super::{OPENROUTER_CLAUDE, OPENROUTER_LUNA, Server, assistant, model};
    use crate::anthropic::scenario::{PROMPTS, desk_instructions, weather, weather_tool};
    use crate::anthropic::{calls, results, user};

    /// A request to a model with nothing in it but a reasoning level.
    pub(crate) fn base(provider: &str, model_id: &str, level: Option<&str>) -> ModelRequest {
        let mut request = ModelRequest::new(model(provider, model_id), ModelRole::Main);
        request.reasoning = Reasoning {
            level: level.map(str::to_owned),
            budget_tokens: None,
            visibility: ReasoningVisibility::Summary,
        };
        request
    }

    /// The least reasoning a server's model can be asked for: none, where
    /// it can be turned off.
    fn least(server: &Server) -> &'static str {
        server.off.unwrap_or(server.low)
    }

    /// Plain text: one short answer.
    pub(crate) fn text(server: &Server) -> ModelRequest {
        let mut request = base(server.provider, server.model, Some(least(server)));
        request.system = "You answer in one short sentence.".to_owned();
        request.messages = vec![user("Say hello to the test suite.")];
        request.max_output = Some(512);
        request
    }

    /// Reasoning, and then the answer.
    pub(crate) fn reasoning(server: &Server) -> ModelRequest {
        let mut request = base(server.provider, server.model, Some(server.high));
        request.messages = vec![user(
            "A farmer has 17 sheep and buys 23 more, then sells a quarter of the flock. \
             How many sheep are left? Think it through briefly before you answer.",
        )];
        request.max_output = Some(2048);
        request
    }

    /// One tool call.
    pub(crate) fn tool_call(server: &Server) -> ModelRequest {
        let mut request = base(server.provider, server.model, Some(server.low));
        request.tools = vec![weather_tool()];
        request.messages = vec![user("What is the weather in Paris? Use the tool.")];
        request.max_output = Some(1024);
        request
    }

    /// Several tool calls in one response.
    pub(crate) fn tool_calls(server: &Server) -> ModelRequest {
        let mut request = base(server.provider, server.model, Some(server.low));
        request.tools = vec![weather_tool()];
        request.messages = vec![user(
            "What is the weather in Paris, in Tokyo and in Lima? Call the tool for all three \
             cities at once, in one response.",
        )];
        request.max_output = Some(1024);
        request
    }

    /// A tool call cut off by the output limit in the middle of its
    /// arguments.
    pub(crate) fn tool_call_cut(server: &Server) -> ModelRequest {
        let mut request = base(server.provider, server.model, Some(least(server)));
        request.tools = vec![weather_tool()];
        request.messages = vec![user(
            "What is the weather in Paris, in Tokyo, in Lima, in Oslo and in Cairo? Call the \
             tool for all five cities at once, in one response, and say nothing else.",
        )];
        request.max_output = Some(28);
        request
    }

    /// The models of the conversation that changes model inside one
    /// provider and changes back: DeepSeek's two.
    pub(crate) const MODELS: &[&str] = &["deepseek-flash", "deepseek-v4-pro", "deepseek-flash"];

    /// A request of that conversation: the model is the next in turn,
    /// by how many responses the history holds.
    pub(crate) fn desk_models(messages: Vec<Message>) -> ModelRequest {
        let answered = messages
            .iter()
            .filter(|message| message.role == cortex_protocol::Role::Assistant)
            .count();
        let model = MODELS[answered.min(MODELS.len() - 1)];
        desk_on(super::providers::DEEPSEEK, model, "low", messages)
    }

    /// A response cut off by the output limit.
    pub(crate) fn max_tokens(server: &Server) -> ModelRequest {
        let mut request = base(server.provider, server.model, Some(least(server)));
        request.messages = vec![user("Write the numbers from 1 to 200, one per line.")];
        request.max_output = Some(24);
        request
    }

    /// A request of a conversation with the weather desk: the same system
    /// prompt, tools and settings every time, and the history given.
    pub(crate) fn desk(server: &Server, messages: Vec<Message>) -> ModelRequest {
        desk_on(server.provider, server.model, server.low, messages)
    }

    /// The same, on a model of the caller's choice.
    pub(crate) fn desk_on(
        provider: &str,
        model_id: &str,
        level: &str,
        messages: Vec<Message>,
    ) -> ModelRequest {
        let mut request = base(provider, model_id, Some(level));
        request.system = desk_instructions();
        request.tools = vec![weather_tool()];
        request.messages = messages;
        request.max_output = Some(1024);
        request.cache = CacheHints {
            stable_prefix: true,
            history: true,
            lifetime: None,
            key: Some("cortex-fixture-session".to_owned()),
        };
        request
    }

    /// The history after a response: the response itself, then the results
    /// of its tool calls, or, when it called none, the user's next prompt.
    /// `None` when the conversation is over.
    pub(crate) fn after(
        mut history: Vec<Message>,
        events: &[ModelEvent],
        request: &ModelRequest,
        provider: &ProviderInfo,
    ) -> Option<Vec<Message>> {
        let reply = assistant(events, request, provider);
        let asked = !calls(&reply).is_empty();
        let next = if asked {
            results(&reply, weather)
        } else {
            let said = history
                .iter()
                .filter(|message| {
                    message.role == cortex_protocol::Role::User
                        && message
                            .parts
                            .iter()
                            .all(|part| matches!(part, cortex_protocol::Part::Text(_)))
                })
                .count();
            user(PROMPTS.get(said)?)
        };
        history.push(reply);
        history.push(next);
        Some(history)
    }

    /// How a conversation with the desk begins.
    pub(crate) fn opening() -> Vec<Message> {
        vec![user(PROMPTS[0])]
    }

    /// How many requests a recorded conversation has at most.
    pub(crate) const ROUNDS: usize = 4;

    /// The history a server is handed when a session changes to it from an
    /// Anthropic model: what was said, the Anthropic model's response with
    /// its reasoning and its tool calls, and the tools' results. It is the
    /// first exchange of the conversation recorded from Anthropic's API.
    pub(crate) fn from_anthropic() -> Vec<Message> {
        use crate::anthropic::scenario as theirs;
        let request = theirs::desk_at(
            theirs::SWITCH[0],
            theirs::SWITCH_LEVEL,
            vec![user(theirs::SWITCH_PROMPT)],
        );
        let events =
            crate::anthropic::decode_whole(&request, &crate::anthropic::load("switch-1").response);
        theirs::after(request.messages.clone(), &events, &request)
            .expect("the Anthropic model asked for tools")
    }

    /// A request at a speed tier, as small as one can be: on OpenRouter's
    /// entry for a model of OpenAI's, which is asked with `service_tier`.
    pub(crate) fn tier(speed: Speed) -> ModelRequest {
        let mut request = base(super::providers::OPENROUTER, OPENROUTER_LUNA, Some("none"));
        request.speed = speed;
        request.messages = vec![user("Reply with the one word: ready")];
        request.max_output = Some(64);
        request
    }

    /// A request at fast speed where a speed is another model: Venice.ai.
    pub(crate) fn fast_model(server: &Server) -> ModelRequest {
        let mut request = base(server.provider, server.model, server.off);
        request.speed = Speed::Fast;
        request.messages = vec![user("Reply with the one word: ready")];
        request.max_output = Some(64);
        request
    }

    /// A request of a conversation with a model of Anthropic's through
    /// OpenRouter: cache markers in the request, and reasoning that comes
    /// back in signed pieces and must be sent back as it came.
    pub(crate) fn claude(messages: Vec<Message>) -> ModelRequest {
        desk_on(
            super::providers::OPENROUTER,
            OPENROUTER_CLAUDE,
            "low",
            messages,
        )
    }
}
