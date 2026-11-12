//! The scripted transport: recorded responses in place of a network
//! (`docs/testing-and-ci.md`, "Deterministic agent tests without a
//! network").
//!
//! A [`ScriptedTransport`] is a [`Transport`] that answers each request with
//! the next [`Reply`] of its script and remembers the requests it was sent.
//! A reply is a status, headers and a body delivered in the chunks the test
//! chooses, and it can also fail to connect, break off part-way, or stall
//! until the request is cancelled or the test opens a [`Gate`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::CancelToken;
use cortex_llm::error::{TransportError, TransportErrorKind};
use cortex_llm::{BodyStream, BoxFuture, Headers, HttpRequest, HttpResponse, Transport};

use crate::gate::Gate;

/// One scripted answer to a request.
#[derive(Debug, Clone)]
pub struct Reply {
    before: Before,
    status: u16,
    headers: Headers,
    body: Vec<Step>,
}

/// What happens before the status and headers arrive.
#[derive(Debug, Clone)]
enum Before {
    /// They arrive at once.
    Nothing,
    /// The request fails without a response.
    Fail(TransportError),
    /// Nothing arrives until the request is cancelled.
    Stall,
}

#[derive(Debug, Clone)]
enum Step {
    Chunk(Vec<u8>),
    Fail(TransportError),
    Stall,
    Wait(Gate),
}

impl Reply {
    /// A response with this status, no headers and an empty body.
    #[must_use]
    pub fn status(status: u16) -> Self {
        Self {
            before: Before::Nothing,
            status,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    /// A successful response whose body is `text`, in one chunk.
    #[must_use]
    pub fn ok(text: impl AsRef<[u8]>) -> Self {
        Self::status(200).chunk(text)
    }

    /// A request that gets no response: the connection fails.
    #[must_use]
    pub fn connect_failure(message: &str) -> Self {
        Self {
            before: Before::Fail(TransportError::new(TransportErrorKind::Connect, message)),
            ..Self::status(0)
        }
    }

    /// A request that fails without a response, in the way given.
    #[must_use]
    pub fn failure(error: TransportError) -> Self {
        Self {
            before: Before::Fail(error),
            ..Self::status(0)
        }
    }

    /// A request whose response never starts: it ends only when the request
    /// is cancelled or given up.
    #[must_use]
    pub fn never_answers() -> Self {
        Self {
            before: Before::Stall,
            ..Self::status(0)
        }
    }

    /// Adds a response header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Adds one chunk to the body.
    #[must_use]
    pub fn chunk(mut self, bytes: impl AsRef<[u8]>) -> Self {
        self.body.push(Step::Chunk(bytes.as_ref().to_vec()));
        self
    }

    /// Adds `bytes` to the body in chunks of `size` bytes, the last one
    /// shorter: the same response as it would arrive over a slow network.
    #[must_use]
    pub fn chunks_of(mut self, size: usize, bytes: impl AsRef<[u8]>) -> Self {
        for chunk in bytes.as_ref().chunks(size.max(1)) {
            self.body.push(Step::Chunk(chunk.to_vec()));
        }
        self
    }

    /// The connection is lost at this point of the body.
    #[must_use]
    pub fn then_disconnect(mut self) -> Self {
        self.body.push(Step::Fail(TransportError::new(
            TransportErrorKind::Body,
            "the connection was lost",
        )));
        self
    }

    /// The body stops here and never goes on. The stream ends only when the
    /// request is cancelled or given up.
    #[must_use]
    pub fn then_stall(mut self) -> Self {
        self.body.push(Step::Stall);
        self
    }

    /// The body waits here until the test opens `gate`.
    #[must_use]
    pub fn then_wait(mut self, gate: &Gate) -> Self {
        self.body.push(Step::Wait(gate.clone()));
        self
    }
}

/// A [`Transport`] that plays a script. Clones share the script and the
/// record of requests.
#[derive(Debug, Clone, Default)]
pub struct ScriptedTransport {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    replies: VecDeque<Reply>,
    requests: Vec<HttpRequest>,
}

impl ScriptedTransport {
    /// A transport that answers with these replies, in order.
    #[must_use]
    pub fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        let transport = Self::default();
        for reply in replies {
            transport.push(reply);
        }
        transport
    }

    /// Adds a reply to the end of the script.
    pub fn push(&self, reply: Reply) {
        self.lock().replies.push_back(reply);
    }

    /// Every request sent so far, in order, as the transport received it:
    /// with the credential headers.
    #[must_use]
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.lock().requests.clone()
    }

    /// The JSON body of each request sent so far.
    #[must_use]
    pub fn bodies(&self) -> Vec<serde_json::Value> {
        self.requests()
            .iter()
            .map(|request| {
                serde_json::from_slice(&request.body).expect("a request body that is JSON")
            })
            .collect()
    }

    /// How many replies have not been used.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.lock().replies.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn cancelled() -> TransportError {
    TransportError::new(TransportErrorKind::Cancelled, "The request was cancelled.")
}

impl Transport for ScriptedTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancel: CancelToken,
    ) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let reply = {
                let mut state = self.lock();
                state.requests.push(request);
                state.replies.pop_front()
            };
            let Some(reply) = reply else {
                return Err(TransportError::new(
                    TransportErrorKind::InvalidRequest,
                    "the scripted transport has no reply left for this request",
                ));
            };
            match reply.before {
                Before::Nothing => {}
                Before::Fail(error) => return Err(error),
                Before::Stall => {
                    cancel.cancelled().await;
                    return Err(cancelled());
                }
            }
            let steps = reply.body;
            let body = BodyStream::generate(move |out| async move {
                for step in steps {
                    match step {
                        Step::Chunk(bytes) => out.emit(Ok(bytes)).await,
                        Step::Fail(error) => {
                            out.emit(Err(error)).await;
                            return;
                        }
                        Step::Stall => {
                            cancel.cancelled().await;
                            out.emit(Err(cancelled())).await;
                            return;
                        }
                        Step::Wait(gate) => {
                            if cancel.race(gate.opened()).await.is_none() {
                                out.emit(Err(cancelled())).await;
                                return;
                            }
                        }
                    }
                }
            });
            Ok(HttpResponse {
                status: reply.status,
                headers: reply.headers,
                body,
            })
        })
    }
}
