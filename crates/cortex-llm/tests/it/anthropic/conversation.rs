//! The recorded conversations, end to end and without a network.
//!
//! A conversation was recorded one request at a time. Here it is rebuilt:
//! the first request from the scenario, each later one from what the
//! decoder made of the response before it. Three things must hold.
//!
//! - **Each rebuilt request is the recorded one, byte for byte.** So what
//!   the decoder stored of a response, sent back by the builder, is exactly
//!   what the real API was sent, and accepted.
//! - **Each request begins with the one before it, byte for byte.** That is
//!   what the provider's cache needs, and its check of reasoning blocks.
//! - **The responses say the cache was read.** From the second request on,
//!   the usage reports the tokens the request before it wrote.

use cortex_llm::anthropic::AnthropicMessages;
use cortex_llm::{ModelEvent, ModelRequest, Protocol};
use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{Message, Part};

use super::scenario::{self, SWITCH, SWITCH_LEVEL, SWITCH_PROMPT};
use super::{HAIKU, SONNET, decode_whole, load, provider, user};

/// A change to a request that was already sent, with what it changes.
type Edit = (&'static str, Box<dyn Fn(&mut ModelRequest)>);

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
}

/// Rebuilds a recorded conversation from its fixtures `<name>-1`,
/// `<name>-2` and so on, one for each model given.
fn rebuild(name: &str, models: &[&str], level: &str, first: &str) -> Vec<Round> {
    let mut history = vec![user(first)];
    let mut rounds = Vec::new();
    for (at, model) in models.iter().enumerate() {
        let fixture = load(&format!("{name}-{}", at + 1));
        let request = scenario::desk_at(model, level, history.clone());
        let built = AnthropicMessages
            .build(&request, &provider())
            .unwrap_or_else(|error| panic!("{error}"))
            .body;
        let events = decode_whole(&request, &fixture.response);
        let next = scenario::after(history, &events, &request);
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
    assert_eq!(
        rounds.len(),
        models.len(),
        "{name}: the conversation ended early"
    );
    rounds
}

fn conversation() -> Vec<Round> {
    rebuild(
        "conversation",
        &[HAIKU, HAIKU, HAIKU, HAIKU],
        "medium",
        scenario::PROMPTS[0],
    )
}

fn switch() -> Vec<Round> {
    rebuild("switch", SWITCH, SWITCH_LEVEL, SWITCH_PROMPT)
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("a request body is text")
}

/// A body without the two brackets that close its messages and itself.
fn open(body: &[u8]) -> &[u8] {
    body.strip_suffix(b"]}")
        .expect("a body ends by closing its messages")
}

/// Where two byte strings first differ, with what is around it, for a
/// failure a person can read.
fn first_difference(earlier: &[u8], later: &[u8]) -> String {
    let at = earlier
        .iter()
        .zip(later)
        .position(|(a, b)| a != b)
        .unwrap_or(earlier.len().min(later.len()));
    let around = |bytes: &[u8]| {
        let from = at.saturating_sub(60);
        let to = (at + 60).min(bytes.len());
        String::from_utf8_lossy(&bytes[from..to]).into_owned()
    };
    format!(
        "they differ at byte {at}:\n  earlier: …{}…\n  later:   …{}…",
        around(earlier),
        around(later)
    )
}

fn assert_begins_with(later: &[u8], earlier: &[u8], what: &str) {
    assert!(
        later.starts_with(earlier),
        "{what}: the later request does not begin with the earlier one; {}",
        first_difference(earlier, later)
    );
}

#[test]
fn each_request_is_the_one_the_real_api_was_sent() {
    for (name, rounds) in [("conversation", conversation()), ("switch", switch())] {
        for (at, round) in rounds.iter().enumerate() {
            assert!(
                round.built == round.recorded,
                "{name}-{}: the builder no longer writes the request that was recorded; {}",
                at + 1,
                first_difference(&round.recorded, &round.built)
            );
        }
    }
}

#[test]
fn each_request_begins_with_the_one_before_it_byte_for_byte() {
    let rounds = conversation();
    for pair in rounds.windows(2) {
        // The bodies as they went over the wire.
        assert_begins_with(&pair[1].recorded, open(&pair[0].recorded), "as recorded");
        assert_begins_with(&pair[1].built, open(&pair[0].built), "as built");
        // And the part the adapter names as stable.
        let earlier = AnthropicMessages
            .stable_prefix(&pair[0].request, &provider())
            .unwrap();
        let later = AnthropicMessages
            .stable_prefix(&pair[1].request, &provider())
            .unwrap();
        assert_begins_with(&later, &earlier, "the stable prefix");
        assert!(
            later.len() > earlier.len(),
            "a later request adds to the earlier one"
        );
        // In an ordinary round the stable prefix is the body without its
        // closing brackets: no marker is inside the messages.
        assert_eq!(earlier, open(&pair[0].built));
    }
    // The tools and the system prompt come before the first message, so
    // that they are the beginning of every request.
    let body = text(&rounds[0].built);
    let (tools, system, messages) = (
        body.find("\"tools\":").unwrap(),
        body.find("\"system\":").unwrap(),
        body.find("\"messages\":").unwrap(),
    );
    assert!(tools < system && system < messages);
    assert!(body.ends_with("]}"));
}

#[test]
fn a_change_to_earlier_content_breaks_the_prefix_and_the_check_sees_it() {
    let rounds = conversation();
    let earlier = AnthropicMessages
        .stable_prefix(&rounds[1].request, &provider())
        .unwrap();

    // Each of these changes something that was already sent.
    let edits: Vec<Edit> = vec![
        (
            "the system prompt",
            Box::new(|request| request.system.push_str(" Today is Friday.")),
        ),
        (
            "a tool's description",
            Box::new(|request| request.tools[0].description.push('!')),
        ),
        (
            "an earlier user message",
            Box::new(|request| request.messages[0] = user("What is the weather in Oslo?")),
        ),
        (
            "an earlier message removed",
            Box::new(|request| {
                request.messages.remove(0);
            }),
        ),
        (
            "a reasoning block left out",
            Box::new(|request| {
                let before = request.messages[1].parts.len();
                request.messages[1]
                    .parts
                    .retain(|part| !matches!(part, Part::Reasoning(_)));
                assert!(
                    request.messages[1].parts.len() < before,
                    "there was one to leave out"
                );
            }),
        ),
        (
            "the stored block of an earlier answer reformatted",
            Box::new(|request| {
                for part in &mut request.messages[1].parts {
                    if let Part::ToolCall(call) = part {
                        let raw = call.raw.as_ref().unwrap().as_str().replace("\":", "\": ");
                        call.raw = Some(cortex_protocol::RawJson::new(raw));
                    }
                }
            }),
        ),
        (
            "the reasoning level",
            Box::new(|request| request.reasoning.level = Some("high".to_owned())),
        ),
        (
            "the cache lifetime",
            Box::new(|request| request.cache.lifetime = Some("1h".to_owned())),
        ),
    ];
    for (what, edit) in edits {
        let mut request = rounds[2].request.clone();
        edit(&mut request);
        let later = AnthropicMessages
            .stable_prefix(&request, &provider())
            .unwrap();
        assert!(
            !later.starts_with(&earlier),
            "{what} changed, and the later request still begins with the earlier one"
        );
    }
    // Adding at the end changes nothing that was sent.
    let mut request = rounds[2].request.clone();
    request.messages.push(user("One more thing."));
    let later = AnthropicMessages
        .stable_prefix(&request, &provider())
        .unwrap();
    assert!(later.starts_with(&earlier));
}

#[test]
fn the_responses_say_the_cache_was_read() {
    let rounds = conversation();
    let first = rounds[0].tokens();
    assert!(
        first.cache_write > 0,
        "the first request wrote the prefix: {first:?}"
    );
    assert_eq!(first.cache_read, 0);
    for pair in rounds.windows(2) {
        let (before, after) = (pair[0].tokens(), pair[1].tokens());
        // Everything the request before it sent is read back, and only what
        // is new is written.
        assert_eq!(
            after.cache_read,
            before.cache_read + before.cache_write,
            "{before:?} then {after:?}"
        );
        assert!(
            after.input < 10,
            "nearly nothing is paid for at the full price"
        );
    }
}

/// The stored blocks of the reasoning parts of a message.
fn reasoning_blocks(message: &Message) -> Vec<&str> {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Reasoning(reasoning) => {
                reasoning.raw.as_ref().map(cortex_protocol::RawJson::as_str)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_change_of_model_and_back_sends_every_reasoning_block_unchanged() {
    let rounds = switch();
    assert_eq!(
        [
            rounds[0].request.model.info.id.as_str(),
            rounds[1].request.model.info.id.as_str(),
            rounds[2].request.model.info.id.as_str()
        ],
        [SONNET, HAIKU, SONNET]
    );
    // The first model reasoned, and so did the second.
    let last = &rounds[2].request;
    let from_first = reasoning_blocks(&last.messages[1]);
    let from_second = reasoning_blocks(&last.messages[3]);
    assert!(!from_first.is_empty(), "the first model wrote no reasoning");
    assert!(
        !from_second.is_empty(),
        "the second model wrote no reasoning"
    );
    assert_eq!(
        last.messages[1].origin.as_ref().unwrap().model.as_str(),
        SONNET
    );
    assert_eq!(
        last.messages[3].origin.as_ref().unwrap().model.as_str(),
        HAIKU
    );
    // The second model cannot read the first one's reasoning, by the
    // catalog. It is sent to it all the same.
    let haiku = &rounds[1].request.model.info;
    assert!(
        !haiku
            .reads_reasoning_from
            .iter()
            .any(|id| id.as_str() == SONNET)
    );
    for block in &from_first {
        assert!(
            text(&rounds[1].built).contains(block),
            "to the second model"
        );
    }
    // Back on the first model, both models' blocks are there, each as the
    // text the decoder stored, in the order they were produced.
    let body = text(&rounds[2].built);
    let mut found = 0;
    for block in from_first.iter().chain(&from_second) {
        let at = body[found..]
            .find(block)
            .unwrap_or_else(|| panic!("a reasoning block is missing or out of order: {block}"));
        found += at + block.len();
    }
    // And those are the requests the real API accepted: it answered both.
    for round in &rounds {
        assert!(round.built == round.recorded);
        assert!(matches!(round.events.last(), Some(ModelEvent::End { .. })));
    }
    // The first model's own cache survived the detour: on the way back it
    // read more than the tools and the system prompt.
    assert!(rounds[2].tokens().cache_read > rounds[1].tokens().cache_read);
}
