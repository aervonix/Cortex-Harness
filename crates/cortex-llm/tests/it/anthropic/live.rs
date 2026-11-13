//! Tier 5: the real API, by hand (`docs/testing-and-ci.md`, "Test tiers").
//!
//! Both tests here are ignored. No CI job runs ignored tests, and nothing
//! here runs unless a person asks for it:
//!
//! ```text
//! cargo xtask live                    the smoke test
//! cargo xtask record [SCENARIO…]      record fixtures; no name records all
//! ```
//!
//! `CORTEX_LIVE_ANTHROPIC_KEY_FILE` names the file that holds the API key.
//! How the key is read, what may be written and how a run's spend is
//! limited are the same for every protocol, and are in
//! [`crate::live`].
//!
//! # What it costs
//!
//! Every scenario is a few hundred tokens on the cheapest model, except the
//! ones that are about a dearer model: the request at fast speed, and the
//! conversation that changes model. Each call prints what it used and what
//! that cost at the catalog's prices, and a run stops before it has spent
//! [`SPEND_LIMIT_NANOS`](crate::live::SPEND_LIMIT_NANOS).

#![allow(
    clippy::disallowed_methods,
    reason = "run by hand: reads the key file and writes fixture files on the test's thread"
)]

use std::sync::Arc;

use cortex_base::Secret;
use cortex_llm::anthropic::{self, AnthropicMessages};
use cortex_llm::{ModelEntry, ModelEvent};
use cortex_protocol::model::SpeedOffer;
use cortex_protocol::{ModelId, Speed};
use cortex_testkit::fixture::Scrubber;

use super::scenario::{self, SWITCH, SWITCH_LEVEL, SWITCH_PROMPT};
use super::{HAIKU, OPUS, fixtures_directory, model, user};
use crate::live::{Live, Target, final_usage, key, nanos_as_dollars, wanted};

/// The variable that names the file the API key is in.
const KEY_FILE: &str = "CORTEX_LIVE_ANTHROPIC_KEY_FILE";

/// Anthropic's own API, as a live session talks to it.
fn target() -> Target {
    Target {
        protocol: Arc::new(AnthropicMessages),
        provider: anthropic::provider(),
        key_file: KEY_FILE,
        auth: |key| Arc::new(anthropic::api_key(key, "a file, for a live test")),
        scrubber: Scrubber::new,
        fixtures: fixtures_directory(),
        record_command: "cargo xtask record",
    }
}

/// A copy of a catalog entry with something changed, for a request the
/// built-in catalog would not let through.
fn altered(id: &str, change: impl FnOnce(&mut ModelEntry)) -> Arc<ModelEntry> {
    let mut entry = ModelEntry::clone(&model(id));
    change(&mut entry);
    Arc::new(entry)
}

/// Runs a conversation with the weather desk on the models given, one
/// request for each, and records each exchange as `<name>-<n>`.
fn converse(
    live: &mut Live,
    name: &str,
    shows: &str,
    models: &[&str],
    level: &str,
    first: &str,
) -> Vec<Vec<ModelEvent>> {
    let mut history = vec![user(first)];
    let mut all = Vec::new();
    for (round, model_id) in models.iter().enumerate() {
        let request = scenario::desk_at(model_id, level, history.clone());
        let events = live.record(&format!("{name}-{}", round + 1), shows, &request);
        let next = scenario::after(history, &events, &request);
        all.push(events);
        match next {
            Some(next) => history = next,
            None => break,
        }
    }
    all
}

#[test]
#[ignore = "tier 5: sends requests to the real API; run it with `cargo xtask live`"]
fn live_smoke() {
    let mut live = Live::start(target());

    // A request, a streamed answer, a usage with tokens in it.
    live.say("smoke: a plain request");
    let (events, _) = live.send(&scenario::text());
    assert!(
        matches!(
            events.first(),
            Some(ModelEvent::Limits(_) | ModelEvent::Start { .. })
        ),
        "{events:?}"
    );
    let usage = final_usage(&events);
    assert!(
        usage.tokens.input > 0 && usage.tokens.output > 0,
        "{usage:?}"
    );
    assert_eq!(usage.speed, Speed::Standard);
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!text.trim().is_empty(), "the model said nothing");

    // Two rounds of a conversation: the second reads the first from the
    // cache, which it can only do if the request's prefix did not change.
    live.say("smoke: a conversation, twice, for the cache");
    let first = scenario::desk(HAIKU, vec![user(scenario::PROMPTS[0])]);
    let (events, _) = live.send(&first);
    let wrote = final_usage(&events).tokens.clone();
    let history =
        scenario::after(first.messages.clone(), &events, &first).expect("the conversation goes on");
    let second = scenario::desk(HAIKU, history);
    let (events, _) = live.send(&second);
    let read = final_usage(&events).tokens.clone();
    assert!(
        wrote.cache_write > 0 || wrote.cache_read > 0,
        "the first request cached nothing: {wrote:?}"
    );
    assert!(
        read.cache_read > 0,
        "the second request read nothing from the cache: {read:?}"
    );
    live.say(&format!(
        "smoke passed. This run cost ${:.6}.",
        nanos_as_dollars(live.spent_nanos)
    ));
}

#[test]
#[ignore = "tier 5: records fixtures from the real API; run it with `cargo xtask record`"]
fn live_record() {
    let known = [
        "text",
        "reasoning",
        "tool-call",
        "tool-calls",
        "max-tokens",
        "conversation",
        "switch",
        "fast",
        "standard-named",
        "errors",
        "speed-not-offered",
        "toolbox",
    ];
    let wanted = wanted(&known);
    let wants = |name: &str| wanted.is_empty() || wanted.iter().any(|wanted| wanted == name);

    let mut live = Live::start(target());
    if wants("toolbox") {
        live.record(
            "toolbox",
            "The whole built-in toolbox, as the binary declares it to this model (reduced for strict mode where the model has it), with a one-line task that needs no tool. It shows the provider accepts the real tools.",
            &crate::toolbox::request("anthropic"),
        );
    }
    if wants("text") {
        live.record("text", "A plain text answer.", &scenario::text());
    }
    if wants("reasoning") {
        live.record(
            "reasoning",
            "Reasoning shown as a summary, with its signature, and then the answer.",
            &scenario::reasoning(),
        );
    }
    if wants("tool-call") {
        live.record("tool-call", "One tool call.", &scenario::tool_call());
    }
    if wants("tool-calls") {
        live.record(
            "tool-calls",
            "Several tool calls in one response.",
            &scenario::tool_calls(),
        );
    }
    if wants("max-tokens") {
        live.record(
            "max-tokens",
            "A response cut off by the output limit: the stop reason `max_tokens`.",
            &scenario::max_tokens(),
        );
    }
    if wants("conversation") {
        converse(
            &mut live,
            "conversation",
            "One request of a conversation with tools, on one model. Each request repeats \
             the one before it and adds to it, and from the second on the response reads \
             the earlier part from the cache.",
            &[HAIKU, HAIKU, HAIKU, HAIKU],
            "medium",
            scenario::PROMPTS[0],
        );
    }
    if wants("switch") {
        converse(
            &mut live,
            "switch",
            "One request of a conversation that changes model and changes back. The second \
             model cannot read the first one's reasoning; every block is sent back unchanged \
             all the same, and the provider leaves out what the model cannot read.",
            SWITCH,
            SWITCH_LEVEL,
            SWITCH_PROMPT,
        );
    }
    if wants("fast") {
        let events = live.record(
            "fast",
            "A request at fast speed. The usage reports the speed that served it, and the \
             headers carry the fast tier's own rate limits.",
            &scenario::fast(),
        );
        if let Some(ModelEvent::End { usage, .. }) = events.last() {
            live.say(&format!(
                "  fast: served at `{}`, asked for {:?}",
                usage.speed.as_str(),
                usage.speed_requested
            ));
        }
    }
    if wants("standard-named") {
        live.record(
            "standard-named",
            "The same request with `speed: \"standard\"` named, as a model that offers a tier \
             is always told.",
            &scenario::standard_named(),
        );
    }
    if wants("errors") {
        // A model that does not exist: 404.
        let mut request = scenario::text();
        request.model = altered(HAIKU, |entry| {
            entry.info.id = ModelId::new("claude-not-a-model-0");
        });
        live.record(
            "error-not-found",
            "A request for a model that does not exist: a failed response.",
            &request,
        );
        // A reasoning level the model does not have: 400.
        let mut request = scenario::text();
        request.model = altered(HAIKU, |entry| {
            entry.info.reasoning.levels.push("utmost".to_owned());
        });
        request.reasoning.level = Some("utmost".to_owned());
        live.record(
            "error-invalid-request",
            "A request the provider refuses as invalid: a reasoning level it does not know.",
            &request,
        );
        // A key that is not one: 401. The real key is still what is guarded.
        let mut wrong = Live::with_keys(
            target(),
            key(KEY_FILE),
            Secret::new("sk-ant-api03-this-is-not-a-key"),
        );
        wrong.record(
            "error-authentication",
            "A request with a key that is not one: a failed response.",
            &scenario::text(),
        );
    }
    if wants("speed-not-offered") {
        // What does a model that has no fast mode say to a speed? The
        // catalog gives it no tier, so the builder sends it none; this entry
        // is altered to find out what the provider would answer.
        let offer: SpeedOffer = model(OPUS).info.speeds[0].clone();
        let mut request = scenario::text();
        request.model = altered(HAIKU, |entry| entry.info.speeds = vec![offer]);
        live.record(
            "error-speed-not-offered",
            "What the provider answers when a request names a speed, even `standard`, for a \
             model that has no fast mode: a 400. It is why the builder leaves `speed` out for \
             a model whose catalog entry offers no tier. The request was made with a catalog \
             entry altered for the purpose; the built-in catalog would not have sent it.",
            &request,
        );
    }
    live.say(&format!(
        "done. This run cost ${:.6}.",
        nanos_as_dollars(live.spent_nanos)
    ));
}
