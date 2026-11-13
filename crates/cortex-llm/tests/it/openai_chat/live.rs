//! Tier 5: the real servers, by hand (`docs/testing-and-ci.md`, "Test
//! tiers").
//!
//! Every test here is ignored. No CI job runs ignored tests, and nothing
//! here runs unless a person asks for it:
//!
//! ```text
//! cargo xtask live PROVIDER                        the smoke test
//! cargo xtask record --provider PROVIDER [SCENARIO…]
//!                                                  record fixtures; no name records all
//! ```
//!
//! `PROVIDER` is `openrouter`, `deepseek` or `venice`. Each has a variable
//! that names the file its API key is in:
//! `CORTEX_LIVE_OPENROUTER_KEY_FILE`, `CORTEX_LIVE_DEEPSEEK_KEY_FILE` and
//! `CORTEX_LIVE_VENICE_KEY_FILE`. How a key is read, what may be written
//! and how a run's spend is limited are in [`crate::live`].
//!
//! # What it costs
//!
//! Every scenario is a few hundred tokens on one of the cheapest models a
//! server has. Recording all of a server's fixtures costs about a cent.
//! Each call prints what it used and what that cost, and a run ends by
//! asking the provider's own usage endpoint how far its figure moved.

#![allow(
    clippy::disallowed_methods,
    reason = "run by hand: reads the key file and writes fixture files on the test's thread"
)]

use std::sync::Arc;

use cortex_base::{Clock as _, Secret, SystemClock};
use cortex_llm::openai_chat::{self, OpenAiChat, account};
use cortex_llm::{ModelEntry, ModelEvent, ModelRequest};
use cortex_protocol::{Message, ModelId, Speed};
use cortex_testkit::fixture::Scrubber;

use super::scenario::{self, ROUNDS};
use super::{DEEPSEEK, OPENROUTER, Server, VENICE, fixtures_directory, model, provider};
use crate::live::{Live, Target, final_usage, key, nanos_as_dollars, wanted};

/// The variable that names the file a server's API key is in, and the
/// command that records its fixtures.
fn names(server: &Server) -> (&'static str, &'static str) {
    match server.provider {
        "openrouter" => (
            "CORTEX_LIVE_OPENROUTER_KEY_FILE",
            "cargo xtask record --provider openrouter",
        ),
        "deepseek" => (
            "CORTEX_LIVE_DEEPSEEK_KEY_FILE",
            "cargo xtask record --provider deepseek",
        ),
        "venice" => (
            "CORTEX_LIVE_VENICE_KEY_FILE",
            "cargo xtask record --provider venice",
        ),
        other => panic!("no live tests are written for `{other}`"),
    }
}

/// A server, as a live session talks to it.
fn target(server: &Server) -> Target {
    let (key_file, record_command) = names(server);
    Target {
        protocol: Arc::new(OpenAiChat),
        provider: provider(server.provider),
        key_file,
        auth: |key| Arc::new(openai_chat::api_key(key, "a file, for a live test")),
        // These servers write identifiers every which way, so they are
        // found by the member they are in.
        scrubber: |key| Scrubber::new(key).with_id_members(&["id", "tool_call_id"]),
        fixtures: fixtures_directory(),
        record_command,
    }
}

/// A copy of a catalog entry with something changed, for a request the
/// built-in catalog would not let through.
fn altered(server: &Server, change: impl FnOnce(&mut ModelEntry)) -> Arc<ModelEntry> {
    let mut entry = ModelEntry::clone(&model(server.provider, server.model));
    change(&mut entry);
    Arc::new(entry)
}

fn text_of(events: &[ModelEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Runs a conversation with the weather desk, one request for each round,
/// and records each exchange as `<name>-<n>`.
fn converse(
    live: &mut Live,
    name: &str,
    shows: &str,
    mut history: Vec<Message>,
    request: impl Fn(Vec<Message>) -> ModelRequest,
    rounds: usize,
) -> Vec<Vec<ModelEvent>> {
    let provider = live.target.provider.clone();
    let mut all = Vec::new();
    for round in 1..=rounds {
        let request = request(history.clone());
        let events = live.record(&format!("{name}-{round}"), shows, &request);
        let next = scenario::after(history, &events, &request, &provider);
        all.push(events);
        match next {
            Some(next) => history = next,
            None => break,
        }
    }
    all
}

/// What the provider's usage endpoint says the account has spent or has
/// left, as one number that a run's spend moves: the amount spent where
/// the provider reports one, and otherwise what is left, negated. `None`
/// when the endpoint reports no dollars.
///
/// Only the difference between two readings is ever printed. The figure
/// itself says something about the account, and is not for a log. The
/// answer itself is never printed or written at all: OpenRouter's holds a
/// shortened form of the key.
fn meter(live: &mut Live) -> Option<i64> {
    if crate::live::dry_run() {
        live.say("dry run: the usage endpoint is not asked");
        return None;
    }
    let request = account::request(&live.target.provider)
        .unwrap_or_else(|error| panic!("{error}"))
        .expect("the provider's entry names a usage endpoint");
    let (status, headers, body) = live.fetch(request);
    let now = SystemClock::new().now();
    let usage = account::read(&live.target.provider, status, &headers, &body, now).unwrap_or_else(
        |error| {
            // Even a failure's message is checked for the key before it is
            // printed.
            let said = format!("the usage endpoint: {} ({})", error.message, error.code);
            match live.scrubber.printable(&said) {
                Ok(said) => panic!("{said}"),
                Err(refused) => panic!("{refused}"),
            }
        },
    );
    let currencies: Vec<&str> = usage
        .balances
        .iter()
        .map(|balance| balance.currency.as_str())
        .collect();
    live.say(&format!(
        "  usage endpoint: status {status}, available={:?}, balances in {currencies:?}, {} limits",
        usage.available,
        usage.limits.len()
    ));
    let dollars = usage
        .balances
        .iter()
        .find(|balance| balance.currency == "USD")?;
    dollars
        .used
        .or_else(|| dollars.remaining.map(|remaining| -remaining))
}

/// Says how far the provider's own figure moved between two readings.
fn say_moved(live: &Live, before: Option<i64>, after: Option<i64>) {
    match (before, after) {
        (Some(before), Some(after)) => {
            #[expect(clippy::cast_precision_loss, reason = "a figure for a person to read")]
            let moved = (after - before) as f64 / 1e9;
            live.say(&format!(
                "  the provider's own figure moved by ${moved:.6} during this run (it may lag)"
            ));
        }
        _ => live.say("  the provider's usage endpoint reports no figure in US dollars"),
    }
}

fn smoke(server: &Server) {
    let mut live = Live::start(target(server));
    let provider = live.target.provider.clone();
    let before = meter(&mut live);

    // A request, a streamed answer, a usage with tokens in it.
    live.say("smoke: a plain request");
    let (events, _) = live.send(&scenario::text(server));
    let usage = final_usage(&events);
    assert!(
        usage.tokens.input + usage.tokens.cache_read > 0 && usage.tokens.output > 0,
        "{usage:?}"
    );
    assert_eq!(usage.speed, Speed::Standard);
    assert!(
        !text_of(&events).trim().is_empty(),
        "the model said nothing"
    );

    // Two rounds of a conversation with tools: the second sends the first
    // response back, reasoning and all, which the server must accept.
    live.say("smoke: a conversation with tools, two rounds");
    let first = scenario::desk(server, scenario::opening());
    let (events, _) = live.send(&first);
    final_usage(&events);
    let history = scenario::after(first.messages.clone(), &events, &first, &provider)
        .expect("the conversation goes on");
    let (events, _) = live.send(&scenario::desk(server, history));
    final_usage(&events);

    // A history that began on an Anthropic model: its reasoning is left
    // out, its text and tool calls are kept, and the server answers.
    live.say("smoke: a history that began on an Anthropic model");
    let (events, _) = live.send(&scenario::desk(server, scenario::from_anthropic()));
    final_usage(&events);
    assert!(
        !text_of(&events).trim().is_empty(),
        "the model said nothing"
    );

    let after = meter(&mut live);
    say_moved(&live, before, after);
    live.say(&format!(
        "smoke passed. This run cost ${:.6} by the tokens it used.",
        nanos_as_dollars(live.spent_nanos)
    ));
}

/// The scenarios every server has.
const COMMON: &[&str] = &[
    "text",
    "reasoning",
    "tool-call",
    "tool-calls",
    "max-tokens",
    "tool-call-cut",
    "conversation",
    "switch",
    "errors",
    "toolbox",
];

fn record(server: &Server, more: &[&str]) {
    let known: Vec<&str> = COMMON.iter().chain(more).copied().collect();
    let wanted = wanted(&known);
    // A scenario runs when this server has it, and it was asked for or
    // nothing in particular was.
    let wants = |name: &str| {
        known.contains(&name) && (wanted.is_empty() || wanted.iter().any(|wanted| wanted == name))
    };
    let at = server.provider;

    let mut live = Live::start(target(server));
    let before = meter(&mut live);
    if wants("toolbox") {
        live.record(
            &format!("{at}-toolbox"),
            "The whole built-in toolbox, as the binary declares it to this model (reduced for strict mode where the model has it), with a one-line task that needs no tool. It shows the provider accepts the real tools.",
            &crate::toolbox::request(at),
        );
    }
    if wants("text") {
        live.record(
            &format!("{at}-text"),
            "A plain text answer.",
            &scenario::text(server),
        );
    }
    if wants("reasoning") {
        live.record(
            &format!("{at}-reasoning"),
            "Reasoning, under this server's name for it, and then the answer.",
            &scenario::reasoning(server),
        );
    }
    if wants("tool-call") {
        live.record(
            &format!("{at}-tool-call"),
            "One tool call.",
            &scenario::tool_call(server),
        );
    }
    if wants("tool-calls") {
        live.record(
            &format!("{at}-tool-calls"),
            "Several tool calls in one response, each keyed by its index.",
            &scenario::tool_calls(server),
        );
    }
    if wants("max-tokens") {
        live.record(
            &format!("{at}-max-tokens"),
            "A response cut off by the output limit: the stop reason `length`.",
            &scenario::max_tokens(server),
        );
    }
    if wants("tool-call-cut") {
        live.record(
            &format!("{at}-tool-call-cut"),
            "A response cut off by the output limit while it was writing tool calls: the stop \
             reason `length`, and a call whose arguments are not finished.",
            &scenario::tool_call_cut(server),
        );
    }
    if wants("models") {
        converse(
            &mut live,
            &format!("{at}-models"),
            "One request of a conversation that changes model inside this provider and changes \
             back. The second model's entry does not say it reads the first one's reasoning, so \
             the first model's response is sent to it without its reasoning, and with the \
             member this server demands left empty.",
            scenario::opening(),
            scenario::desk_models,
            scenario::MODELS.len(),
        );
    }
    if wants("conversation") {
        converse(
            &mut live,
            &format!("{at}-conversation"),
            "One request of a conversation with tools, on one model. Each request repeats the \
             one before it and adds to it, and sends the model's own earlier responses back as \
             they came, reasoning included.",
            scenario::opening(),
            |history| scenario::desk(server, history),
            ROUNDS,
        );
    }
    if wants("switch") {
        converse(
            &mut live,
            &format!("{at}-switch"),
            "One request of a conversation that began on an Anthropic model and changed to \
             this one. The Anthropic model's response is sent as its neutral projection: its \
             text and its tool calls, without its reasoning.",
            scenario::from_anthropic(),
            |history| scenario::desk(server, history),
            2,
        );
    }
    if wants("errors") {
        // A model that does not exist.
        let mut request = scenario::text(server);
        request.model = altered(server, |entry| {
            entry.info.id = ModelId::new("not-a-model-0");
        });
        live.record(
            &format!("{at}-error-not-found"),
            "A request for a model that does not exist: a failed response.",
            &request,
        );
        // A request the server refuses as invalid. The servers differ in
        // what they check, so two are tried: a tool whose schema is not
        // one, and an output limit of nothing. A request a server accepts
        // is not a fixture of a failure, and is not kept.
        let mut bad_schema = scenario::tool_call(server);
        bad_schema.tools[0].schema = serde_json::json!({"type": "no-such-type", "properties": 7});
        let mut no_output = scenario::text(server);
        no_output.max_output = Some(0);
        let name = format!("{at}-error-invalid-request");
        for (shows, request) in [
            (
                "A request the server refuses as invalid: a tool whose schema is not one.",
                bad_schema,
            ),
            (
                "A request the server refuses as invalid: an output limit of no tokens.",
                no_output,
            ),
        ] {
            live.record(&name, shows, &request);
            let directory = fixtures_directory().join(&name);
            let status = cortex_testkit::fixture::Fixture::load(&directory)
                .unwrap_or_else(|error| panic!("{error}"))
                .status;
            if status != 200 {
                break;
            }
            std::fs::remove_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
            live.say("  the server answered with a 200: not kept as a failed response");
        }
        // A key that is not one. The real key is still what is guarded.
        let mut wrong = Live::with_keys(
            target(server),
            key(names(server).0),
            Secret::new("sk-this-is-not-a-key-0000000000000000"),
        );
        wrong.record(
            &format!("{at}-error-authentication"),
            "A request with a key that is not one: a failed response.",
            &scenario::text(server),
        );
    }
    if wants("error-after-200") {
        // Venice.ai accepts a tool whose schema is not one, begins the
        // response, and then reports the failure inside it.
        let mut request = scenario::tool_call(server);
        request.tools[0].schema = serde_json::json!({"type": "no-such-type", "properties": 7});
        live.record(
            &format!("{at}-error-after-200"),
            "A response that begins with a successful status and then fails: the server \
             accepted a tool whose schema is not one, started to answer, and sent an `error` \
             in the stream.",
            &request,
        );
    }
    if wants("tier") {
        let events = live.record(
            &format!("{at}-fast"),
            "A request at fast speed, asked for with `service_tier`. The response says which \
             tier served it.",
            &scenario::tier(Speed::Fast),
        );
        if let Some(ModelEvent::End { usage, .. }) = events.last() {
            live.say(&format!(
                "  fast: served at `{}`, asked for {:?}",
                usage.speed.as_str(),
                usage.speed_requested
            ));
        }
        live.record(
            &format!("{at}-standard-named"),
            "The same request with `service_tier: \"default\"` named, as a model that offers a \
             tier is always told.",
            &scenario::tier(Speed::Standard),
        );
    }
    if wants("fast-model") {
        live.record(
            &format!("{at}-fast"),
            "A request at fast speed at a server that sells the speed as another model: the \
             request names that model's id.",
            &scenario::fast_model(server),
        );
    }
    if wants("claude") {
        converse(
            &mut live,
            &format!("{at}-claude"),
            "One request of a conversation with a model of Anthropic's through this server. \
             The request carries cache markers, and the reasoning comes back in signed pieces \
             (`reasoning_details`) that are sent back as they came.",
            scenario::opening(),
            scenario::claude,
            3,
        );
    }
    let after = meter(&mut live);
    say_moved(&live, before, after);
    live.say(&format!(
        "done. This run cost ${:.6} by the tokens it used.",
        nanos_as_dollars(live.spent_nanos)
    ));
}

#[test]
#[ignore = "tier 5: sends requests to the real API; run it with `cargo xtask live openrouter`"]
fn live_smoke_openrouter() {
    smoke(&OPENROUTER);
}

#[test]
#[ignore = "tier 5: sends requests to the real API; run it with `cargo xtask live deepseek`"]
fn live_smoke_deepseek() {
    smoke(&DEEPSEEK);
}

#[test]
#[ignore = "tier 5: sends requests to the real API; run it with `cargo xtask live venice`"]
fn live_smoke_venice() {
    smoke(&VENICE);
}

#[test]
#[ignore = "tier 5: records fixtures from the real API; run it with `cargo xtask record --provider openrouter`"]
fn live_record_openrouter() {
    record(&OPENROUTER, &["tier", "claude"]);
}

#[test]
#[ignore = "tier 5: records fixtures from the real API; run it with `cargo xtask record --provider deepseek`"]
fn live_record_deepseek() {
    record(&DEEPSEEK, &["models"]);
}

#[test]
#[ignore = "tier 5: records fixtures from the real API; run it with `cargo xtask record --provider venice`"]
fn live_record_venice() {
    record(&VENICE, &["fast-model", "error-after-200"]);
}
