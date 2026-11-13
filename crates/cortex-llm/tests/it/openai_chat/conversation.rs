//! The recorded conversations, end to end and without a network.
//!
//! A conversation was recorded one request at a time. Here it is rebuilt:
//! the first request from the scenario, each later one from what the
//! decoder made of the response before it. Three things must hold.
//!
//! - **Each rebuilt request is the recorded one, byte for byte.** So what
//!   the decoder stored of a response, sent back by the builder, is exactly
//!   what the real server was sent, and accepted: reasoning under the
//!   server's own name, signed pieces as they came, and the empty member
//!   one server demands.
//! - **Each request begins with the one before it, byte for byte.** That is
//!   what a server's cache needs.
//! - **The responses say the cache was read**, at the servers that report
//!   it.
//!
//! And the done criterion of the step: **a session changes from an
//! Anthropic model to a compatible one and back**, with reasoning dropped
//! and everything else kept. The change to the compatible server was sent
//! to all three real servers and is recorded; the change back is built
//! here for Anthropic's adapter, whose own tests cover what it sends.

use cortex_llm::anthropic::AnthropicMessages;
use cortex_llm::openai_chat::OpenAiChat;
use cortex_llm::{ModelEvent, ModelRequest, Protocol, ProviderInfo};
use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{Message, Part, Role};
use cortex_testkit::fixture::pretty;
use serde_json::{Value, json};

use super::scenario::{self, ROUNDS};
use super::{
    DEEPSEEK, OPENROUTER, RECORDED, Server, VENICE, decode_whole, fixture_names, load, provider,
};
use crate::anthropic::assert_snapshot;

/// One request of a rebuilt conversation, with what was recorded for it.
struct Round {
    request: ModelRequest,
    /// The body the builder writes for the request today.
    built: Vec<u8>,
    /// The body that was sent when the conversation was recorded.
    recorded: Vec<u8>,
    events: Vec<ModelEvent>,
}

impl Round {
    fn tokens(&self) -> &TokenUsage {
        match self.events.last() {
            Some(ModelEvent::End { usage, .. }) => &usage.tokens,
            other => panic!("the response did not complete: {other:?}"),
        }
    }

    fn body(&self) -> Value {
        serde_json::from_slice(&self.built).expect("a body is JSON")
    }
}

/// A recorded conversation: the fixtures `<name>-1`, `<name>-2` and so on,
/// the history it began with, and how each of its requests is made.
struct Recorded {
    name: String,
    provider: ProviderInfo,
    opening: Vec<Message>,
    request: Box<dyn Fn(Vec<Message>) -> ModelRequest>,
}

impl Recorded {
    fn conversation(server: &'static Server) -> Self {
        Self {
            name: format!("{}-conversation", server.provider),
            provider: provider(server.provider),
            opening: scenario::opening(),
            request: Box::new(move |history| scenario::desk(server, history)),
        }
    }

    fn switch(server: &'static Server) -> Self {
        Self {
            name: format!("{}-switch", server.provider),
            provider: provider(server.provider),
            opening: scenario::from_anthropic(),
            request: Box::new(move |history| scenario::desk(server, history)),
        }
    }

    fn claude() -> Self {
        Self {
            name: "openrouter-claude".to_owned(),
            provider: provider(OPENROUTER.provider),
            opening: scenario::opening(),
            request: Box::new(scenario::claude),
        }
    }

    /// The conversation that changes model inside DeepSeek and back.
    fn models() -> Self {
        Self {
            name: "deepseek-models".to_owned(),
            provider: provider(DEEPSEEK.provider),
            opening: scenario::opening(),
            request: Box::new(scenario::desk_models),
        }
    }

    /// Every recorded conversation.
    fn all() -> Vec<Self> {
        let mut all = vec![Self::models()];
        for server in RECORDED {
            all.push(Self::conversation(server));
            all.push(Self::switch(server));
        }
        all.push(Self::claude());
        all
    }

    /// Rebuilds the conversation from its fixtures. Returns the rounds and
    /// the history after the last of them.
    fn rebuild(&self) -> (Vec<Round>, Vec<Message>) {
        let names = fixture_names();
        let mut history = self.opening.clone();
        let mut rounds = Vec::new();
        for at in 1..=ROUNDS {
            let name = format!("{}-{at}", self.name);
            if !names.contains(&name) {
                break;
            }
            let fixture = load(&name);
            let request = (self.request)(history.clone());
            let built = OpenAiChat
                .build(&request, &self.provider)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .body;
            let events = decode_whole(&request, &self.provider, &fixture.response);
            let next = scenario::after(history.clone(), &events, &request, &self.provider);
            rounds.push(Round {
                request,
                built,
                recorded: fixture.request,
                events,
            });
            match next {
                Some(next) => history = next,
                None => break,
            }
        }
        assert!(rounds.len() >= 2, "{}: {} rounds", self.name, rounds.len());
        (rounds, history)
    }
}

#[test]
fn every_rebuilt_request_is_the_one_that_was_recorded() {
    let mut compared = 0;
    for conversation in Recorded::all() {
        let (rounds, _) = conversation.rebuild();
        for (at, round) in rounds.iter().enumerate() {
            assert!(
                round.built == round.recorded,
                "{}-{}: what the decoder stored and the builder sent back is not what was \
                 recorded.\n--- recorded\n{}\n--- built\n{}",
                conversation.name,
                at + 1,
                pretty(&String::from_utf8_lossy(&round.recorded)),
                pretty(&String::from_utf8_lossy(&round.built)),
            );
            compared += 1;
        }
    }
    // Every conversation fixture on disk was one of them.
    let on_disk = fixture_names()
        .iter()
        .filter(|name| {
            name.contains("-conversation-")
                || name.contains("-switch-")
                || name.contains("-claude-")
                || name.contains("-models-")
        })
        .count();
    assert_eq!(compared, on_disk);
}

#[test]
fn each_request_begins_with_the_one_before_it_byte_for_byte() {
    for conversation in Recorded::all() {
        let (rounds, _) = conversation.rebuild();
        for pair in rounds.windows(2) {
            // A change of model changes the request from its first member
            // on: there is no prefix to keep across it.
            if pair[0].request.model.info.id != pair[1].request.model.info.id {
                continue;
            }
            let earlier = OpenAiChat
                .stable_prefix(&pair[0].request, &conversation.provider)
                .unwrap();
            let later = &pair[1].built;
            assert!(
                later.starts_with(&earlier),
                "{}: a later request does not begin with the one before it",
                conversation.name
            );
            // The prefix is the whole body without its two closing
            // brackets, and the later request is longer.
            assert_eq!(&pair[0].built[..earlier.len()], &earlier[..]);
            assert_eq!(&pair[0].built[earlier.len()..], b"]}");
            assert!(later.len() > earlier.len() + 2);
        }
    }
}

#[test]
fn a_change_to_earlier_content_breaks_the_prefix_and_the_test_can_tell() {
    let conversation = Recorded::conversation(&DEEPSEEK);
    let (rounds, _) = conversation.rebuild();
    // The second request holds the first response, and the third begins
    // with the second.
    let earlier = OpenAiChat
        .stable_prefix(&rounds[1].request, &conversation.provider)
        .unwrap();
    assert!(rounds[2].built.starts_with(&earlier));
    type Edit = (&'static str, Box<dyn Fn(&mut ModelRequest)>);
    let edits: Vec<Edit> = vec![
        (
            "the system prompt",
            Box::new(|request| request.system.push('!')),
        ),
        (
            "the tools",
            Box::new(|request| request.tools[0].description.push('!')),
        ),
        (
            "the first message",
            Box::new(|request| request.messages[0] = crate::anthropic::user("Something else.")),
        ),
        (
            "the reasoning level",
            Box::new(|request| request.reasoning.level = Some("high".to_owned())),
        ),
        (
            "the stored reasoning of the first response",
            Box::new(|request| {
                for part in &mut request.messages[1].parts {
                    if let Part::Reasoning(reasoning) = part {
                        reasoning.raw = None;
                    }
                }
            }),
        ),
    ];
    for (what, edit) in edits {
        let mut later = rounds[2].request.clone();
        edit(&mut later);
        let built = OpenAiChat
            .build(&later, &conversation.provider)
            .unwrap()
            .body;
        assert!(
            !built.starts_with(&earlier),
            "a change to {what} went unnoticed"
        );
    }
}

#[test]
fn the_servers_that_report_a_cache_say_it_was_read() {
    // DeepSeek and Venice.ai cache by themselves; Claude through
    // OpenRouter caches where the request marks it.
    for conversation in [
        Recorded::conversation(&DEEPSEEK),
        Recorded::conversation(&VENICE),
        Recorded::claude(),
    ] {
        let (rounds, _) = conversation.rebuild();
        for round in &rounds[1..] {
            let tokens = round.tokens();
            assert!(
                tokens.cache_read > 0,
                "{}: nothing was read from the cache: {tokens:?}",
                conversation.name
            );
            assert!(
                tokens.cache_read > tokens.input,
                "{}: {tokens:?}",
                conversation.name
            );
        }
    }
    let (rounds, _) = Recorded::claude().rebuild();
    assert!(rounds[0].tokens().cache_write > 0);
    assert_eq!(
        rounds[1].tokens().cache_read,
        rounds[0].tokens().cache_write
    );
}

/// The assistant messages of a body.
fn assistants(body: &Value) -> Vec<&Value> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "assistant")
        .collect()
}

#[test]
fn a_models_own_response_goes_back_with_its_reasoning_as_it_came() {
    // DeepSeek and Venice.ai: under `reasoning_content`.
    for server in [&DEEPSEEK, &VENICE] {
        let (rounds, _) = Recorded::conversation(server).rebuild();
        let thought: String = rounds[0]
            .events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(!thought.is_empty(), "{server:?}");
        let body = rounds[1].body();
        let sent_back = assistants(&body)[0];
        assert_eq!(sent_back["reasoning_content"], json!(thought), "{server:?}");
        assert!(
            sent_back["tool_calls"]
                .as_array()
                .is_some_and(|calls| !calls.is_empty())
        );
    }
    // Claude through OpenRouter: the signed pieces, whole.
    let (rounds, _) = Recorded::claude().rebuild();
    let body = rounds[1].body();
    let sent_back = assistants(&body)[0];
    let details = sent_back["reasoning_details"].as_array().unwrap();
    assert_eq!(details.len(), 1);
    assert_eq!(details[0]["type"], "reasoning.text");
    assert!(
        details[0]["signature"]
            .as_str()
            .is_some_and(|signature| !signature.is_empty())
    );
    assert_eq!(details[0]["text"], sent_back["reasoning"]);
}

#[test]
fn a_session_changes_from_an_anthropic_model_to_a_compatible_one() {
    // What the Anthropic model produced: reasoning with a signature, and
    // tool calls.
    let opening = scenario::from_anthropic();
    let anthropic = &opening[1];
    assert_eq!(anthropic.role, Role::Assistant);
    let reasoned = anthropic
        .parts
        .iter()
        .filter(|part| matches!(part, Part::Reasoning(_)))
        .count();
    let called: Vec<&cortex_protocol::message::ToolCallPart> = anthropic
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert!(
        reasoned > 0 && called.len() == 2,
        "the recorded conversation changed"
    );

    for server in RECORDED {
        let conversation = Recorded::switch(server);
        let (rounds, _) = conversation.rebuild();
        // The real server was sent this request, and answered it.
        assert_eq!(rounds[0].built, rounds[0].recorded, "{server:?}");
        let (stop, _) = match rounds[0].events.last() {
            Some(ModelEvent::End { stop, raw_stop, .. }) => (stop, raw_stop),
            other => panic!("{server:?}: the server did not answer: {other:?}"),
        };
        assert!(stop.is_known(), "{server:?}");

        let text = String::from_utf8(rounds[0].built.clone()).unwrap();
        // Reasoning dropped: nothing of the Anthropic model's thinking, its
        // signature or its block types is in the request.
        for gone in [
            "\"thinking\"",
            "signature",
            "\"tool_use\"",
            "\"type\":\"text\",\"text\"",
        ] {
            assert!(!text.contains(gone), "{server:?}: {gone} was sent");
        }
        // Everything else kept: both calls, with their ids, names and
        // arguments, and each result naming its call.
        let body = rounds[0].body();
        let message = assistants(&body)[0];
        let sent: Vec<(&str, &str, Value)> = message["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| {
                (
                    call["id"].as_str().unwrap(),
                    call["function"]["name"].as_str().unwrap(),
                    serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap(),
                )
            })
            .collect();
        let kept: Vec<(&str, &str, Value)> = called
            .iter()
            .map(|call| (call.id.as_str(), call.name.as_str(), call.arguments.clone()))
            .collect();
        assert_eq!(sent, kept, "{server:?}");
        let results: Vec<&str> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| message["tool_call_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            results,
            [called[0].id.as_str(), called[1].id.as_str()],
            "{server:?}"
        );
        // The one server that demands its reasoning member gets it empty.
        let demanded = message.get("reasoning_content");
        if server.provider == DEEPSEEK.provider {
            assert_eq!(demanded, Some(&json!("")));
        } else {
            assert_eq!(demanded, None, "{server:?}");
        }
    }
}

#[test]
fn and_back_to_the_anthropic_model_with_reasoning_dropped_and_everything_else_kept() {
    use crate::anthropic::scenario as theirs;
    let conversation = Recorded::switch(&DEEPSEEK);
    let (rounds, history) = conversation.rebuild();
    // The history now holds a response of each: the Anthropic model's, and
    // the compatible model's, which reasoned.
    let compatible: Vec<&Message> = history
        .iter()
        .filter(|message| {
            message
                .origin
                .as_ref()
                .is_some_and(|origin| origin.protocol == cortex_llm::openai_chat::NAME)
        })
        .collect();
    assert_eq!(compatible.len(), rounds.len());
    let its_reasoning: Vec<&str> = compatible
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            Part::Reasoning(reasoning) => Some(reasoning.text.as_str()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect();
    assert!(!its_reasoning.is_empty(), "the compatible model reasoned");
    let its_text: Vec<&str> = compatible
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!its_text.is_empty());

    // Back to the model the session began on.
    let back = theirs::desk_at(theirs::SWITCH[0], theirs::SWITCH_LEVEL, history.clone());
    let http = AnthropicMessages
        .build(&back, &crate::anthropic::provider())
        .unwrap_or_else(|error| panic!("{error}"));
    let text = String::from_utf8(http.body.clone()).unwrap();
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_snapshot(
        "openai-chat/request/switch-back-to-anthropic.txt",
        &pretty(&text),
    );
    // The compatible model's reasoning is dropped: not its text, and not
    // the member it travelled in.
    for reasoning in &its_reasoning {
        assert!(
            !text.contains(&reasoning[..reasoning.len().min(40)]),
            "its reasoning was sent"
        );
    }
    assert!(!text.contains("reasoning_content"));
    // Its text is kept.
    for said in &its_text {
        let block = json!({"type": "text", "text": said});
        assert!(
            body["messages"].as_array().unwrap().iter().any(|message| {
                message["role"] == "assistant"
                    && message["content"]
                        .as_array()
                        .is_some_and(|blocks| blocks.contains(&block))
            }),
            "{said} was not sent"
        );
    }
    // And the Anthropic model's own response goes back as it came, with
    // its reasoning and its signature: it is its own provider's again.
    let first = &body["messages"][1];
    assert_eq!(first["role"], "assistant");
    let types: Vec<&str> = first["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["type"].as_str().unwrap())
        .collect();
    assert!(
        types.contains(&"thinking") && types.contains(&"tool_use"),
        "{types:?}"
    );
    assert!(
        first["content"][0]["signature"]
            .as_str()
            .is_some_and(|signature| !signature.is_empty())
    );

    // And once more to the compatible server: its own responses go back
    // raw again, and the Anthropic one as its projection, exactly as
    // before. The request it was sent then is the beginning of this one.
    let again = scenario::desk(&DEEPSEEK, history);
    let built = OpenAiChat
        .build(&again, &conversation.provider)
        .unwrap()
        .body;
    let earlier = OpenAiChat
        .stable_prefix(&rounds.last().unwrap().request, &conversation.provider)
        .unwrap();
    assert!(built.starts_with(&earlier));
}

#[test]
fn a_change_of_model_inside_one_provider_drops_the_reasoning_and_the_server_takes_it() {
    // Recorded from DeepSeek: its first model, then its second, then the
    // first again. Neither model's entry says it reads the other's
    // reasoning, so each is sent the other's responses without it.
    let (rounds, _) = Recorded::models().rebuild();
    assert_eq!(rounds.len(), 3);
    let models: Vec<&str> = rounds
        .iter()
        .map(|round| round.request.model.info.id.as_str())
        .collect();
    assert_eq!(models, scenario::MODELS);
    let thought = |round: &Round| -> String {
        round
            .events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    };
    assert!(!thought(&rounds[0]).is_empty(), "the first model reasoned");

    // To the second model: the first one's response has its text and its
    // calls, and the member this server demands, empty. The real server
    // answered it.
    let body = rounds[1].body();
    let first = assistants(&body)[0];
    assert_eq!(first["reasoning_content"], "");
    assert!(
        first["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
    );
    assert!(matches!(
        rounds[1].events.last(),
        Some(ModelEvent::End { .. })
    ));

    // Back to the first model: its own response goes back as it came,
    // reasoning and all, and the second model's goes without.
    let body = rounds[2].body();
    let sent = assistants(&body);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["reasoning_content"], json!(thought(&rounds[0])));
    assert_eq!(sent[1]["reasoning_content"], "");
    assert!(matches!(
        rounds[2].events.last(),
        Some(ModelEvent::End { .. })
    ));
}
