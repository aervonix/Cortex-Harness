//! What the agent loop sees of a model, and the one implementation of it
//! (`docs/architecture/04-providers.md`, "Traits").
//!
//! [`ModelClient`] is the whole interface: a request in, a stream of events
//! out. The agent loop is generic over it and is tested with the scripted
//! provider of `cortex-testkit`.
//!
//! [`ProviderClient`] is the real client. It is written once, for every
//! protocol, by putting together a [`Protocol`], a [`Transport`], an
//! [`AuthMethod`] and the retry layer:
//!
//! 1. Decide the speed to send: the one asked for, or `standard` when the
//!    model does not offer it or its cooldown is running.
//! 2. Build the request, add the credential, send it, and decode the
//!    response stream into events, under two time limits: one for the first
//!    event and one for silence after it.
//! 3. On a failure, ask the [`RetryPolicy`] what to do: wait and send
//!    again, fall back to standard speed and start a cooldown, or report
//!    the error.
//!
//! Every step that waits can be cancelled, and none of it reads a real
//! clock: time comes from a [`Clock`](cortex_base::Clock) and a [`Timer`].

use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::{CancelToken, Rng, SharedClock, SystemRng};
use cortex_protocol::Speed;
use cortex_protocol::speed::{CooldownState, SpeedFallbackBy, SpeedFallbackReason};

use crate::auth::{AuthMethod, NoAuth};
use crate::error::{ProviderError, TransportErrorKind, code};
use crate::event::{CooldownChange, EventSink, ModelEvent, RetryInfo, SpeedMove};
use crate::http::HttpResponse;
use crate::image::{ImageSource, Inlined, inline_images};
use crate::protocol::{HttpFailure, Protocol, ProviderInfo};
use crate::request::ModelRequest;
use crate::retry::{Decision, RetryPolicy, RetryState};
use crate::speed::{CooldownStatus, SpeedCooldowns};
use crate::sse::SseParser;
use crate::stream::{Emitter, ModelStream};
use crate::time::{Raced, Timer, race};
use crate::transport::Transport;

/// A model, as the agent loop sees it.
pub trait ModelClient: Send + Sync {
    /// Sends one request and returns its response as a stream of events.
    ///
    /// Nothing is sent until the stream is first polled. The stream's last
    /// event is [`ModelEvent::End`] or [`ModelEvent::Error`]. Cancelling
    /// `cancel` ends it with an error of kind `Interrupted`; dropping the
    /// stream abandons the request.
    fn stream(&self, request: ModelRequest, cancel: CancelToken) -> ModelStream;
}

/// How much of a failed response's body is read for its error message.
const FAILURE_BODY_LIMIT: usize = 64 * 1024;

/// The client for one provider: a protocol, a transport and the retry
/// layer.
///
/// Clones are the same client.
#[derive(Debug, Clone)]
pub struct ProviderClient {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    protocol: Arc<dyn Protocol>,
    provider: ProviderInfo,
    transport: Arc<dyn Transport>,
    auth: Arc<dyn AuthMethod>,
    /// Replaced whole when the configuration is reloaded. A request keeps
    /// the policy it started with.
    policy: Mutex<Arc<RetryPolicy>>,
    clock: SharedClock,
    timer: Arc<dyn Timer>,
    rng: Mutex<Box<dyn Rng>>,
    cooldowns: SpeedCooldowns,
    images: Option<Arc<dyn ImageSource>>,
}

/// Puts a [`ProviderClient`] together.
#[derive(Debug)]
pub struct ProviderClientBuilder {
    protocol: Arc<dyn Protocol>,
    provider: ProviderInfo,
    transport: Arc<dyn Transport>,
    clock: SharedClock,
    timer: Arc<dyn Timer>,
    auth: Arc<dyn AuthMethod>,
    policy: RetryPolicy,
    rng: Box<dyn Rng>,
    cooldowns: Option<SpeedCooldowns>,
    images: Option<Arc<dyn ImageSource>>,
}

impl ProviderClientBuilder {
    /// The credential to add to each request. Without one, none is added.
    #[must_use]
    pub fn auth(mut self, auth: Arc<dyn AuthMethod>) -> Self {
        self.auth = auth;
        self
    }

    /// The retry policy. Without one, the documented defaults.
    #[must_use]
    pub fn policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The source of backoff jitter. Without one, the system's.
    #[must_use]
    pub fn rng(mut self, rng: Box<dyn Rng>) -> Self {
        self.rng = rng;
        self
    }

    /// The table of speed-tier cooldowns. Give every client of a process
    /// the same table, and the agent loop too, so that what is shown is
    /// what is obeyed. Without one, the client keeps a table of its own.
    #[must_use]
    pub fn cooldowns(mut self, cooldowns: SpeedCooldowns) -> Self {
        self.cooldowns = Some(cooldowns);
        self
    }

    /// Where stored images are loaded from. A request can refer to an
    /// image by the artifact that holds it; the client puts the image's
    /// bytes in the request it sends (`crate::image`). Without a source, a
    /// request that refers to a stored image fails.
    #[must_use]
    pub fn images(mut self, images: Arc<dyn ImageSource>) -> Self {
        self.images = Some(images);
        self
    }

    /// The client.
    #[must_use]
    pub fn build(self) -> ProviderClient {
        let cooldowns = self
            .cooldowns
            .unwrap_or_else(|| SpeedCooldowns::new(Arc::clone(&self.clock)));
        ProviderClient {
            inner: Arc::new(Inner {
                protocol: self.protocol,
                provider: self.provider,
                transport: self.transport,
                auth: self.auth,
                policy: Mutex::new(Arc::new(self.policy)),
                clock: self.clock,
                timer: self.timer,
                rng: Mutex::new(self.rng),
                cooldowns,
                images: self.images,
            }),
        }
    }
}

impl ProviderClient {
    /// Starts a client for `provider`, which speaks `protocol`, over
    /// `transport`. Time comes from `clock` and waits from `timer`.
    #[must_use]
    pub fn builder(
        protocol: Arc<dyn Protocol>,
        provider: ProviderInfo,
        transport: Arc<dyn Transport>,
        clock: SharedClock,
        timer: Arc<dyn Timer>,
    ) -> ProviderClientBuilder {
        ProviderClientBuilder {
            protocol,
            provider,
            transport,
            clock,
            timer,
            auth: Arc::new(NoAuth),
            policy: RetryPolicy::default(),
            rng: Box::new(SystemRng::new()),
            cooldowns: None,
            images: None,
        }
    }

    /// The provider this client talks to.
    #[must_use]
    pub fn provider(&self) -> &ProviderInfo {
        &self.inner.provider
    }

    /// The table of speed-tier cooldowns this client starts and obeys.
    #[must_use]
    pub fn cooldowns(&self) -> &SpeedCooldowns {
        &self.inner.cooldowns
    }

    /// The retry policy new requests will use.
    #[must_use]
    pub fn policy(&self) -> Arc<RetryPolicy> {
        self.inner.policy()
    }

    /// Replaces the retry policy, for the next request. A request that is
    /// running keeps the policy it began with.
    pub fn set_policy(&self, policy: RetryPolicy) {
        *self
            .inner
            .policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(policy);
    }
}

impl ModelClient for ProviderClient {
    fn stream(&self, request: ModelRequest, cancel: CancelToken) -> ModelStream {
        let inner = Arc::clone(&self.inner);
        ModelStream::generate(move |out| run(inner, request, cancel, out))
    }
}

impl Inner {
    fn policy(&self) -> Arc<RetryPolicy> {
        Arc::clone(&self.policy.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// How one attempt ended.
enum Attempt {
    /// The response is complete and its `End` has been handed over.
    Complete,
    /// The request was cancelled.
    Cancelled,
    /// The attempt failed. Nothing has been said about it yet.
    Failed(ProviderError),
}

/// One request, from the choice of speed to its last event.
async fn run(
    inner: Arc<Inner>,
    mut request: ModelRequest,
    cancel: CancelToken,
    out: Emitter<ModelEvent>,
) {
    let policy = inner.policy();
    let requested = request.speed.clone();
    let provider = inner.provider.id.clone();
    let model = request.model.info.id.clone();

    // The speed to send. The harness's own reasons for another one are
    // said before anything is sent.
    let mut sent = requested.clone();
    if !requested.is_standard() {
        let moved = |reason, cooldown_until| {
            ModelEvent::SpeedFallback(SpeedMove {
                requested: requested.clone(),
                served: Speed::Standard,
                by: SpeedFallbackBy::Harness,
                reason,
                cooldown_until,
            })
        };
        if request.model.info.offers(&requested) {
            match inner.cooldowns.check(&provider, &model, &requested) {
                CooldownStatus::Clear => {}
                CooldownStatus::Cooling { until } => {
                    sent = Speed::Standard;
                    out.emit(moved(SpeedFallbackReason::Cooldown, Some(until)))
                        .await;
                }
                CooldownStatus::Ended => {
                    out.emit(ModelEvent::SpeedCooldown(CooldownChange {
                        speed: requested.clone(),
                        state: CooldownState::Ended,
                        until: None,
                    }))
                    .await;
                }
            }
        } else {
            sent = Speed::Standard;
            out.emit(moved(SpeedFallbackReason::NotOffered, None)).await;
        }
    }

    // Images that are stored as artifacts are loaded once, for every
    // attempt, into this copy of the request.
    match inline_images(&mut request, inner.images.as_deref(), &cancel).await {
        Inlined::Done => {}
        Inlined::Cancelled => {
            out.emit(ModelEvent::Error(ProviderError::interrupted()))
                .await;
            return;
        }
        Inlined::Failed(error) => {
            out.emit(ModelEvent::Error(error)).await;
            return;
        }
    }

    let mut state = RetryState::new();
    loop {
        state.begin_attempt();
        request.speed = sent.clone();
        let error = match attempt(&inner, &request, &requested, &policy, &cancel, &out).await {
            Attempt::Complete => return,
            Attempt::Cancelled => {
                out.emit(ModelEvent::Error(ProviderError::interrupted()))
                    .await;
                return;
            }
            Attempt::Failed(error) => error,
        };

        let decision = {
            let mut rng = inner.rng.lock().unwrap_or_else(PoisonError::into_inner);
            policy.decide(&mut state, &error, &sent, rng.as_mut())
        };
        match decision {
            Decision::Wait(wait) => {
                let now = inner.clock.now();
                let sleep = inner.timer.sleep(wait);
                out.emit(ModelEvent::Retry(RetryInfo {
                    attempt: state.attempts().saturating_add(1),
                    max_attempts: policy.max_attempts,
                    error,
                    wait,
                    next_attempt_at: now.checked_add(wait).unwrap_or(now),
                }))
                .await;
                if cancel.race(sleep).await.is_none() {
                    out.emit(ModelEvent::Error(ProviderError::interrupted()))
                        .await;
                    return;
                }
            }
            Decision::FallBack { reason, cooldown } => {
                let until = inner.cooldowns.start(&provider, &model, &sent, cooldown);
                out.emit(ModelEvent::SpeedFallback(SpeedMove {
                    requested: requested.clone(),
                    served: Speed::Standard,
                    by: SpeedFallbackBy::Harness,
                    reason,
                    cooldown_until: Some(until),
                }))
                .await;
                out.emit(ModelEvent::SpeedCooldown(CooldownChange {
                    speed: sent.clone(),
                    state: CooldownState::Started,
                    until: Some(until),
                }))
                .await;
                sent = Speed::Standard;
            }
            Decision::GiveUp => {
                out.emit(ModelEvent::Error(error)).await;
                return;
            }
        }
    }
}

/// One send of the request, at the speed in `request.speed`, and the
/// decoding of its response. Events are handed over as they are decoded.
async fn attempt(
    inner: &Inner,
    request: &ModelRequest,
    requested: &Speed,
    policy: &RetryPolicy,
    cancel: &CancelToken,
    out: &Emitter<ModelEvent>,
) -> Attempt {
    if cancel.is_cancelled() {
        return Attempt::Cancelled;
    }
    let mut sink = EventSink::new(requested.clone(), request.speed.clone());

    let mut http = match inner.protocol.build(request, &inner.provider) {
        Ok(http) => http,
        Err(error) => return Attempt::Failed(error.into()),
    };
    http.headers.merge(&inner.provider.headers);
    // The credential goes on last, so nothing before this point holds it.
    match cancel.race(inner.auth.authorize(&mut http)).await {
        None => return Attempt::Cancelled,
        Some(Err(error)) => return Attempt::Failed(error.into()),
        Some(Ok(())) => {}
    }

    let first_event_limit = inner
        .provider
        .first_event_timeout
        .unwrap_or(policy.first_event_timeout);
    let idle_limit = inner.provider.idle_timeout.unwrap_or(policy.idle_timeout);
    let timed_out = |code: &str, limit: std::time::Duration, what: &str| {
        ProviderError::new(
            cortex_base::ErrorKind::Transient,
            code,
            format!("{what} within {} seconds.", limit.as_secs_f64()),
        )
    };

    // One limit runs from the send to the first event of the response.
    let sent_at = inner.clock.monotonic();
    let mut first_event = inner.timer.sleep(first_event_limit);
    let sending = inner.transport.send(http, cancel.clone());
    let response = match race(sending, cancel, &mut first_event).await {
        Raced::Cancelled => return Attempt::Cancelled,
        Raced::TimedOut => {
            return Attempt::Failed(timed_out(
                code::FIRST_EVENT_TIMEOUT,
                first_event_limit,
                "The provider did not answer",
            ));
        }
        Raced::Done(Err(error)) if error.kind == TransportErrorKind::Cancelled => {
            return Attempt::Cancelled;
        }
        Raced::Done(Err(error)) => return Attempt::Failed(error.into()),
        Raced::Done(Ok(response)) => response,
    };
    let HttpResponse {
        status,
        headers,
        mut body,
    } = response;

    if !(200..300).contains(&status) {
        // Read what the provider says about the failure, within the same
        // limit and up to a size. Whatever arrives is enough to classify.
        let mut text = Vec::new();
        while text.len() < FAILURE_BODY_LIMIT {
            match race(body.next(), cancel, &mut first_event).await {
                Raced::Cancelled => return Attempt::Cancelled,
                Raced::Done(Some(Ok(chunk))) => {
                    let room = FAILURE_BODY_LIMIT - text.len();
                    text.extend_from_slice(&chunk[..chunk.len().min(room)]);
                }
                Raced::TimedOut | Raced::Done(None | Some(Err(_))) => break,
            }
        }
        let failure = HttpFailure {
            status,
            headers: &headers,
            body: &text,
            now: inner.clock.now(),
        };
        return Attempt::Failed(inner.protocol.classify(&failure, request));
    }

    inner
        .protocol
        .read_headers(&headers, inner.clock.now(), &mut sink);
    // What the headers said is handed over, and is not yet the response.
    if let Some(ended) = forward(&mut sink, out).await.ended {
        return ended;
    }

    let mut decoder = inner.protocol.decoder(request, &inner.provider);
    let mut parser = SseParser::new();
    // The limit on the first event, until there is one. `at_ceiling` says
    // that what is left of it is the last of all the request may wait.
    let mut first_event = Some(first_event);
    let mut at_ceiling = false;
    let longest = first_event_limit.saturating_mul(QUEUE_PATIENCE);
    let failed = |sink: &EventSink, error: ProviderError| {
        let id = sink.known_request_id().map(str::to_owned);
        Attempt::Failed(if error.request_id.is_none() {
            error.with_request_id(id)
        } else {
            error
        })
    };

    loop {
        let chunk = match first_event.as_mut() {
            Some(limit) => match race(body.next(), cancel, limit).await {
                Raced::TimedOut => {
                    let error = timed_out(
                        code::FIRST_EVENT_TIMEOUT,
                        if at_ceiling {
                            longest
                        } else {
                            first_event_limit
                        },
                        "The response did not begin",
                    );
                    return failed(&sink, error);
                }
                Raced::Cancelled => return Attempt::Cancelled,
                Raced::Done(chunk) => chunk,
            },
            // From the first event on, the limit is on silence.
            None => {
                let mut idle = inner.timer.sleep(idle_limit);
                match race(body.next(), cancel, &mut idle).await {
                    Raced::TimedOut => {
                        let error = timed_out(
                            code::IDLE_TIMEOUT,
                            idle_limit,
                            "The response stream sent nothing",
                        );
                        return failed(&sink, error);
                    }
                    Raced::Cancelled => return Attempt::Cancelled,
                    Raced::Done(chunk) => chunk,
                }
            }
        };
        let bytes = match chunk {
            None => break,
            Some(Err(error)) if error.kind == TransportErrorKind::Cancelled => {
                return Attempt::Cancelled;
            }
            Some(Err(error)) => return failed(&sink, error.into()),
            Some(Ok(bytes)) => bytes,
        };

        // Bytes that are not yet an event show that the connection is
        // alive: a server that has queued the request sends keep-alive
        // comments until it begins. The wait for the first event starts
        // again, so a queue is waited out and a dead connection is not.
        // But not for ever: a server that only ever says it is alive is
        // given up on after `QUEUE_PATIENCE` limits in all.
        if let Some(limit) = first_event.as_mut() {
            let waited = inner.clock.monotonic().saturating_sub(sent_at);
            let left = longest.saturating_sub(waited);
            at_ceiling = left <= first_event_limit;
            if left.is_zero() {
                let error = timed_out(
                    code::FIRST_EVENT_TIMEOUT,
                    longest,
                    "The response did not begin",
                );
                return failed(&sink, error);
            }
            *limit = inner.timer.sleep(first_event_limit.min(left));
        }
        parser.push(&bytes);
        loop {
            let event = match parser.next_event() {
                None => break,
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    let error = ProviderError::new(
                        cortex_base::ErrorKind::Transient,
                        code::MALFORMED_STREAM,
                        format!("The response stream could not be read: {error}."),
                    );
                    return failed(&sink, error);
                }
            };
            if let Err(error) = decoder.feed(event, &mut sink) {
                return failed(&sink, error.into());
            }
        }
        let forwarded = forward(&mut sink, out).await;
        if let Some(ended) = forwarded.ended {
            return ended;
        }
        if forwarded.any {
            first_event = None;
        }
    }

    // The body is over. A decoder may have been holding something back.
    if let Err(error) = decoder.finish(&mut sink) {
        return failed(&sink, error.into());
    }
    if let Some(ended) = forward(&mut sink, out).await.ended {
        return ended;
    }
    let error = ProviderError::new(
        cortex_base::ErrorKind::Transient,
        code::INCOMPLETE_STREAM,
        "The response stream ended before the response was complete.",
    );
    failed(&sink, error)
}

/// How many first-event limits a request may wait for its first event in
/// all, while bytes that are not an event keep arriving: ten, which is ten
/// minutes at the default limit. One server documents that it holds a
/// queued request for up to ten minutes and then closes the connection
/// itself; a server that sends keep-alives for longer than that is not
/// going to answer.
const QUEUE_PATIENCE: u32 = 10;

/// What handing over the sink's events came to.
struct Forwarded {
    /// Whether any event was handed over.
    any: bool,
    /// How the attempt ended, if one of the events ended it.
    ended: Option<Attempt>,
}

/// Hands the events a decoder wrote to the reader, up to the one that ends
/// the attempt. An error is not handed over: whether the reader hears of it
/// is for the retry layer to decide.
async fn forward(sink: &mut EventSink, out: &Emitter<ModelEvent>) -> Forwarded {
    let mut any = false;
    for event in sink.take() {
        match event {
            ModelEvent::Error(error) => {
                return Forwarded {
                    any,
                    ended: Some(Attempt::Failed(error)),
                };
            }
            ModelEvent::End { .. } => {
                out.emit(event).await;
                return Forwarded {
                    any: true,
                    ended: Some(Attempt::Complete),
                };
            }
            other => {
                any = true;
                out.emit(other).await;
            }
        }
    }
    Forwarded { any, ended: None }
}
