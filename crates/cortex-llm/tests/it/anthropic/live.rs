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
//! # The key
//!
//! `CORTEX_LIVE_ANTHROPIC_KEY_FILE` names a file that holds the API key and
//! nothing else. The key is read from that file by this code and goes into
//! a [`Secret`]: it is never an argument, never in the environment, and
//! never printed. Without the variable a test says so and fails. It does
//! not pass, and it does not skip.
//!
//! # What may be written
//!
//! Everything a test here writes or prints goes through the
//! [`Scrubber`](cortex_testkit::fixture::Scrubber): a fixture is made of a
//! scrubbed exchange, and a line is printed only after it was checked for
//! the key. A recording that still holds the key, or part of it, is refused
//! and nothing of it is written.
//!
//! # What it costs
//!
//! Every scenario is a few hundred tokens on the cheapest model, except the
//! ones that are about a dearer model: the request at fast speed, and the
//! conversation that changes model. Each call prints what it used and what
//! that cost at the catalog's prices, and a run stops before it has spent
//! [`SPEND_LIMIT_NANOS`].

#![allow(
    clippy::disallowed_methods,
    reason = "run by hand: reads the key file and writes fixture files on the test's thread"
)]
#![allow(
    clippy::print_stdout,
    reason = "run by hand: what a call used and cost is what the person running it reads"
)]

use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::{CancelToken, Clock as _, Secret, SystemClock};
use cortex_llm::anthropic::{self, AnthropicMessages};
use cortex_llm::error::TransportError;
use cortex_llm::usage::request_usage;
use cortex_llm::{
    BodyStream, BoxFuture, Catalog, Headers, HttpRequest, HttpResponse, HttpTransport, ModelClient,
    ModelEntry, ModelEvent, ModelRequest, ProviderClient, RetryPolicy, TokioTimer, Transport,
};
use cortex_protocol::model::SpeedOffer;
use cortex_protocol::usage::Billing;
use cortex_protocol::{ModelId, Speed};
use cortex_testkit::fixture::Scrubber;

use super::scenario::{self, SWITCH, SWITCH_LEVEL, SWITCH_PROMPT};
use super::{HAIKU, OPUS, fixtures_directory, model, user};

/// The variable that names the file the API key is in.
const KEY_FILE: &str = "CORTEX_LIVE_ANTHROPIC_KEY_FILE";

/// The variable that names the scenarios to record, separated by commas.
/// Unset or empty records every scenario.
const SCENARIOS: &str = "CORTEX_LIVE_SCENARIOS";

/// A run stops before it has spent this much: half a US dollar.
const SPEND_LIMIT_NANOS: u64 = 500_000_000;

/// One request and its response, as the network saw them.
#[derive(Debug, Clone, Default)]
struct Exchange {
    request: Vec<u8>,
    status: u16,
    headers: Headers,
    body: Vec<u8>,
}

/// A transport that keeps what passes through it. It keeps the body of a
/// request and never its headers, which is where the credential is.
#[derive(Debug, Clone)]
struct Recording {
    inner: Arc<HttpTransport>,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
}

impl Transport for Recording {
    fn send(
        &self,
        request: HttpRequest,
        cancel: CancelToken,
    ) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let sent = request.body.clone();
            let response = self.inner.send(request, cancel).await?;
            let HttpResponse {
                status,
                headers,
                mut body,
            } = response;
            let at = {
                let mut exchanges = self
                    .exchanges
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                exchanges.push(Exchange {
                    request: sent,
                    status,
                    headers: headers.clone(),
                    body: Vec::new(),
                });
                exchanges.len() - 1
            };
            let exchanges = Arc::clone(&self.exchanges);
            let body = BodyStream::generate(move |out| async move {
                while let Some(chunk) = body.next().await {
                    if let Ok(bytes) = &chunk {
                        exchanges.lock().unwrap_or_else(PoisonError::into_inner)[at]
                            .body
                            .extend_from_slice(bytes);
                    }
                    out.emit(chunk).await;
                }
            });
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}

/// A session with the real API.
struct Live {
    client: ProviderClient,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
    scrubber: Scrubber,
    spent_nanos: u64,
    today: String,
}

/// Reads the key from the file the environment names. The panic messages
/// name the variable and the reason, and never anything that was read.
fn key() -> Secret {
    let Some(path) = std::env::var_os(KEY_FILE) else {
        panic!(
            "{KEY_FILE} is not set. A live test needs it to name a file that holds the API \
             key. Nothing was sent."
        );
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        panic!("the file {KEY_FILE} names could not be read. Nothing was sent.");
    };
    let key = text.trim();
    assert!(
        !key.is_empty(),
        "the file {KEY_FILE} names is empty. Nothing was sent."
    );
    Secret::new(key)
}

impl Live {
    fn start() -> Self {
        Self::with_key(key())
    }

    /// A session that sends `sent` as its key and guards the real one.
    fn with_keys(real: Secret, sent: Secret) -> Self {
        let exchanges = Arc::new(Mutex::new(Vec::new()));
        let transport = Recording {
            inner: Arc::new(HttpTransport::default()),
            exchanges: Arc::clone(&exchanges),
        };
        let client = ProviderClient::builder(
            Arc::new(AnthropicMessages),
            anthropic::provider(),
            Arc::new(transport),
            Arc::new(SystemClock::new()),
            Arc::new(TokioTimer),
        )
        .auth(Arc::new(anthropic::api_key(
            sent,
            "a file, for a live test",
        )))
        // One send for each request: a live test never retries by itself.
        .policy(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        })
        .build();
        let today = SystemClock::new().now().to_string()[..10].to_owned();
        Self {
            client,
            exchanges,
            scrubber: Scrubber::new(real),
            spent_nanos: 0,
            today,
        }
    }

    fn with_key(key: Secret) -> Self {
        let sent = Secret::new(key.expose());
        Self::with_keys(key, sent)
    }

    /// Prints a line, unless it holds the key.
    fn say(&self, line: &str) {
        match self.scrubber.printable(line) {
            Ok(line) => println!("{line}"),
            Err(error) => panic!("{error}"),
        }
    }

    /// Sends one request and reads its response to the end. Returns the
    /// events, and the exchanges the request took: more than one when a
    /// refused speed tier was followed by a send at standard speed.
    fn send(&mut self, request: &ModelRequest) -> (Vec<ModelEvent>, Vec<Exchange>) {
        assert!(
            self.spent_nanos < SPEND_LIMIT_NANOS,
            "this run has spent its limit; nothing more is sent"
        );
        let before = self
            .exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let events: Vec<ModelEvent> = runtime.block_on(
            self.client
                .stream(request.clone(), CancelToken::new())
                .collect(),
        );
        let exchanges = self
            .exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)[before..]
            .to_vec();
        self.account(request, &events);
        (events, exchanges)
    }

    /// Prints what a call used and cost, and adds it to the run's spend.
    fn account(&mut self, request: &ModelRequest, events: &[ModelEvent]) {
        let info = &request.model.info;
        let asked = request.speed.as_str();
        match events.last() {
            Some(ModelEvent::End {
                usage, raw_stop, ..
            }) => {
                let version = Catalog::builtin().map_or("", Catalog::version);
                let record = request_usage(
                    usage.clone(),
                    info,
                    request.role.clone(),
                    Billing::Metered,
                    version,
                );
                let cost = record.cost.as_ref().map_or(0, |cost| cost.usd_nanos);
                self.spent_nanos += cost;
                let tokens = &record.tokens;
                self.say(&format!(
                    "  {} asked={asked} served={} stop={raw_stop} input={} cache_read={} \
                     cache_write={} output={} reasoning={:?} cost=${:.6} (run total ${:.6})",
                    info.id,
                    record.speed.as_str(),
                    tokens.input,
                    tokens.cache_read,
                    tokens.cache_write,
                    tokens.output,
                    tokens.reasoning,
                    nanos_as_dollars(cost),
                    nanos_as_dollars(self.spent_nanos),
                ));
            }
            Some(ModelEvent::Error(error)) => {
                self.say(&format!(
                    "  {} asked={asked} failed: kind={} code={} status={:?} retry_after={:?} \
                     tier={:?} (no charge)",
                    info.id, error.kind, error.code, error.status, error.retry_after, error.tier,
                ));
            }
            other => panic!("the stream ended with {other:?}"),
        }
        for event in events {
            match event {
                ModelEvent::SpeedFallback(moved) => self.say(&format!(
                    "  speed moved: asked={} served={} by={:?} reason={:?} cooldown_until={:?}",
                    moved.requested.as_str(),
                    moved.served.as_str(),
                    moved.by,
                    moved.reason,
                    moved.cooldown_until,
                )),
                ModelEvent::Limits(limits) => {
                    for limit in limits {
                        self.say(&format!(
                            "  limit: {} speed={:?} limit={:?} remaining={:?} resets_at={:?}",
                            limit.measure.as_str(),
                            limit.speed.as_ref().map(Speed::as_str),
                            limit.limit,
                            limit.remaining,
                            limit.resets_at.as_ref().map(ToString::to_string),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    /// Sends a request and writes its last exchange as the fixture `name`.
    /// An earlier exchange of the same request (a refusal that was followed
    /// by a fallback) is written as `<name>-refused`.
    fn record(&mut self, name: &str, shows: &str, request: &ModelRequest) -> Vec<ModelEvent> {
        self.say(&format!("{name}:"));
        let (events, exchanges) = self.send(request);
        let count = exchanges.len();
        for (at, exchange) in exchanges.iter().enumerate() {
            let name = if at + 1 == count {
                name.to_owned()
            } else {
                format!("{name}-refused")
            };
            let note = format!(
                "# {name}\n\n{shows}\n\n- Recorded on {} from the real API, with `cargo xtask \
                 record {name}`.\n- Model: `{}`. Speed asked for: `{}`.\n- Identifiers and opaque \
                 payloads are stand-ins, and only the response headers that describe the \
                 response are kept (`docs/testing-and-ci.md`, \"Fixtures\").\n",
                self.today,
                request.model.info.id,
                request.speed.as_str(),
            );
            let fixture = self
                .scrubber
                .fixture(
                    &exchange.request,
                    exchange.status,
                    &exchange.headers,
                    &exchange.body,
                    &note,
                )
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            fixture
                .write(&fixtures_directory().join(&name))
                .unwrap_or_else(|error| panic!("{error}"));
            self.say(&format!(
                "  wrote {name}: status {}, {} bytes of response",
                fixture.status,
                fixture.response.len()
            ));
        }
        events
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a figure for a person to read; the sums are kept in whole nano-dollars"
)]
fn nanos_as_dollars(nanos: u64) -> f64 {
    nanos as f64 / 1e9
}

/// A copy of a catalog entry with something changed, for a request the
/// built-in catalog would not let through.
fn altered(id: &str, change: impl FnOnce(&mut ModelEntry)) -> Arc<ModelEntry> {
    let mut entry = ModelEntry::clone(&model(id));
    change(&mut entry);
    Arc::new(entry)
}

/// The final usage of a response that completed.
fn final_usage(events: &[ModelEvent]) -> &cortex_llm::Usage {
    match events.last() {
        Some(ModelEvent::End { usage, .. }) => usage,
        other => panic!("the response did not complete: {other:?}"),
    }
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
    let mut live = Live::start();

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
    let wanted: Vec<String> = std::env::var(SCENARIOS)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    let wants = |name: &str| wanted.is_empty() || wanted.iter().any(|wanted| wanted == name);
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
    ];
    for name in &wanted {
        assert!(
            known.contains(&name.as_str()),
            "there is no scenario `{name}`. The scenarios are: {}",
            known.join(", ")
        );
    }

    let mut live = Live::start();
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
        let mut wrong = Live::with_keys(key(), Secret::new("sk-ant-api03-this-is-not-a-key"));
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
