//! What the tier-5 tests of every protocol share: a session with a real
//! provider that keeps what passes through it (`docs/testing-and-ci.md`,
//! "Test tiers").
//!
//! The tests themselves are in each protocol's own `live` module, and all
//! of them are ignored: no CI job runs ignored tests, and nothing here runs
//! unless a person asks for it with `cargo xtask live` or `cargo xtask
//! record`.
//!
//! # The key
//!
//! Each provider has a variable that names a **file** holding its API key
//! and nothing else. The key is read from that file by this code and goes
//! into a [`Secret`]: it is never an argument, never in the environment,
//! and never printed. Without the variable a test says so and fails. It
//! does not pass, and it does not skip.
//!
//! # What may be written
//!
//! Everything a test writes or prints goes through the
//! [`Scrubber`](cortex_testkit::fixture::Scrubber): a fixture is made of a
//! scrubbed exchange, and a line is printed only after it was checked for
//! the key. A recording that still holds the key, or part of it, is refused
//! and nothing of it is written.
//!
//! # What it costs
//!
//! Each call prints what it used and what that cost: the provider's own
//! figure where its response states one, and otherwise the catalog's
//! prices. A run stops before it has spent [`SPEND_LIMIT_NANOS`].

#![allow(
    clippy::disallowed_methods,
    reason = "run by hand: reads the key file and writes fixture files on the test's thread"
)]
#![allow(
    clippy::print_stdout,
    reason = "run by hand: what a call used and cost is what the person running it reads"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::{CancelToken, Clock as _, Secret, SystemClock};
use cortex_llm::error::TransportError;
use cortex_llm::usage::request_usage;
use cortex_llm::{
    AuthMethod, BodyStream, BoxFuture, Catalog, Headers, HttpRequest, HttpResponse, HttpTransport,
    ModelClient, ModelEvent, ModelRequest, Protocol, ProviderClient, ProviderInfo, RetryPolicy,
    TokioTimer, Transport,
};
use cortex_protocol::Speed;
use cortex_protocol::usage::Billing;
use cortex_testkit::fixture::Scrubber;

/// The variable that names the scenarios to record, separated by commas.
/// Unset or empty records every scenario.
pub(crate) const SCENARIOS: &str = "CORTEX_LIVE_SCENARIOS";

/// A run stops before it has spent this much: half a US dollar.
pub(crate) const SPEND_LIMIT_NANOS: u64 = 500_000_000;

/// One request and its response, as the network saw them.
#[derive(Debug, Clone, Default)]
pub(crate) struct Exchange {
    pub(crate) request: Vec<u8>,
    pub(crate) status: u16,
    pub(crate) headers: Headers,
    pub(crate) body: Vec<u8>,
}

/// A transport that keeps what passes through it. It keeps the body of a
/// request and never its headers, which is where the credential is.
#[derive(Debug, Clone)]
pub(crate) struct Recording {
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

/// The provider a live session talks to, and how.
pub(crate) struct Target {
    /// The wire dialect.
    pub(crate) protocol: Arc<dyn Protocol>,
    /// The endpoint.
    pub(crate) provider: ProviderInfo,
    /// The variable that names the file the API key is in.
    pub(crate) key_file: &'static str,
    /// How a key is sent to this provider.
    pub(crate) auth: fn(Secret) -> Arc<dyn AuthMethod>,
    /// What stands between a recording and the repository, for a key.
    pub(crate) scrubber: fn(Secret) -> Scrubber,
    /// Where the protocol's fixtures are.
    pub(crate) fixtures: PathBuf,
    /// The command that records a fixture, without the scenario's name:
    /// what a fixture's note says it was made with.
    pub(crate) record_command: &'static str,
}

/// A session with a real provider.
pub(crate) struct Live {
    pub(crate) target: Target,
    client: ProviderClient,
    transport: Recording,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
    pub(crate) scrubber: Scrubber,
    pub(crate) spent_nanos: u64,
    today: String,
}

/// Reads a key from the file the environment names. The panic messages
/// name the variable and the reason, and never anything that was read.
pub(crate) fn key(variable: &str) -> Secret {
    let Some(path) = std::env::var_os(variable) else {
        panic!(
            "{variable} is not set. A live test needs it to name a file that holds the API \
             key. Nothing was sent."
        );
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        panic!("the file {variable} names could not be read. Nothing was sent.");
    };
    let key = text.trim();
    assert!(
        !key.is_empty(),
        "the file {variable} names is empty. Nothing was sent."
    );
    Secret::new(key)
}

/// The scenarios a recorder was asked for, checked against the ones it
/// knows. Empty means all of them.
pub(crate) fn wanted(known: &[&str]) -> Vec<String> {
    let wanted: Vec<String> = std::env::var(SCENARIOS)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    for name in &wanted {
        assert!(
            known.contains(&name.as_str()),
            "there is no scenario `{name}`. The scenarios are: {}",
            known.join(", ")
        );
    }
    wanted
}

impl Live {
    pub(crate) fn start(target: Target) -> Self {
        let real = key(target.key_file);
        let sent = Secret::new(real.expose());
        Self::with_keys(target, real, sent)
    }

    /// A session that sends `sent` as its key and guards the real one.
    pub(crate) fn with_keys(target: Target, real: Secret, sent: Secret) -> Self {
        let exchanges = Arc::new(Mutex::new(Vec::new()));
        let transport = Recording {
            inner: Arc::new(HttpTransport::default()),
            exchanges: Arc::clone(&exchanges),
        };
        let client = ProviderClient::builder(
            Arc::clone(&target.protocol),
            target.provider.clone(),
            Arc::new(transport.clone()),
            Arc::new(SystemClock::new()),
            Arc::new(TokioTimer),
        )
        .auth((target.auth)(sent))
        // One send for each request: a live test never retries by itself.
        .policy(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        })
        .build();
        let today = SystemClock::new().now().to_string()[..10].to_owned();
        let scrubber = (target.scrubber)(real);
        Self {
            target,
            client,
            transport,
            exchanges,
            scrubber,
            spent_nanos: 0,
            today,
        }
    }

    /// Prints a line, unless it holds the key.
    pub(crate) fn say(&self, line: &str) {
        match self.scrubber.printable(line) {
            Ok(line) => println!("{line}"),
            Err(error) => panic!("{error}"),
        }
    }

    /// Sends one request and reads its response to the end. Returns the
    /// events, and the exchanges the request took: more than one when a
    /// refused speed tier was followed by a send at standard speed.
    pub(crate) fn send(&mut self, request: &ModelRequest) -> (Vec<ModelEvent>, Vec<Exchange>) {
        assert!(
            self.spent_nanos < SPEND_LIMIT_NANOS,
            "this run has spent its limit; nothing more is sent"
        );
        let before = self.taken();
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

    fn taken(&self) -> usize {
        self.exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Sends a request that is not a model call (a usage endpoint, a
    /// context probe) with the session's credential, and returns the
    /// status, the headers and the whole body.
    pub(crate) fn fetch(&mut self, mut request: HttpRequest) -> (u16, Headers, Vec<u8>) {
        let auth = (self.target.auth)(Secret::new(key(self.target.key_file).expose()));
        let transport = self.transport.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async move {
            auth.authorize(&mut request)
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            let response = transport
                .send(request, CancelToken::new())
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            let HttpResponse {
                status,
                headers,
                mut body,
            } = response;
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                bytes.extend_from_slice(&chunk.unwrap_or_else(|error| panic!("{error}")));
            }
            (status, headers, bytes)
        })
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
                let stated = usage.provider_cost.is_some();
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
                     cache_write={} output={} reasoning={:?} cost=${:.6}{} (run total ${:.6})",
                    info.id,
                    record.speed.as_str(),
                    tokens.input,
                    tokens.cache_read,
                    tokens.cache_write,
                    tokens.output,
                    tokens.reasoning,
                    nanos_as_dollars(cost),
                    if stated {
                        " as the provider states"
                    } else {
                        ""
                    },
                    nanos_as_dollars(self.spent_nanos),
                ));
            }
            Some(ModelEvent::Error(error)) => {
                self.say(&format!(
                    "  {} asked={asked} failed: kind={} code={} status={:?} retry_after={:?} \
                     tier={:?} message={:?} (no charge counted)",
                    info.id,
                    error.kind,
                    error.code,
                    error.status,
                    error.retry_after,
                    error.tier,
                    error.message,
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
    pub(crate) fn record(
        &mut self,
        name: &str,
        shows: &str,
        request: &ModelRequest,
    ) -> Vec<ModelEvent> {
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
                "# {name}\n\n{shows}\n\n- Recorded on {} from the real API, with `{}`.\n- Model: \
                 `{}`. Speed asked for: `{}`.\n- Identifiers and opaque payloads are stand-ins, \
                 and only the response headers that describe the response are kept \
                 (`docs/testing-and-ci.md`, \"Fixtures\").\n",
                self.today,
                self.target.record_command,
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
                .write(&self.target.fixtures.join(&name))
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
pub(crate) fn nanos_as_dollars(nanos: u64) -> f64 {
    nanos as f64 / 1e9
}

/// The final usage of a response that completed.
pub(crate) fn final_usage(events: &[ModelEvent]) -> &cortex_llm::Usage {
    match events.last() {
        Some(ModelEvent::End { usage, .. }) => usage,
        other => panic!("the response did not complete: {other:?}"),
    }
}
