//! The front ends connected to a core, and the requests the core has put
//! to them (`docs/architecture/11-rpc-protocol.md`, "Client roles",
//! "Server → client requests").
//!
//! A client has a role and capabilities, which it states when it
//! connects, and a queue ([`EventQueue`]) that everything for it goes
//! into. A request to the user (an approval) goes to every client that may
//! answer it, and the first answer wins.
//!
//! **Who may answer.** Only a client with the `user` role. A `supervisor`
//! is another session's orchestration layer, which may answer within its
//! own grant once task leases exist (step 37); until then it is asked
//! nothing and its answers are refused, as an `observer`'s always are. An
//! answer from a client that was not asked is refused too.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_protocol::error::code;
use cortex_protocol::jsonrpc::RequestId;
use cortex_protocol::methods::{ApprovalResponse, ClientCapabilities, ClientRole, ServerRequest};
use cortex_protocol::{ErrorData, ErrorKind, SessionId};
use tokio::sync::oneshot;

use crate::events::EventQueue;

/// A connected front end, by the number the core gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub(crate) u64);

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client {}", self.0)
    }
}

/// What a front end is, and what it can handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// What it is to the sessions it opens.
    pub role: ClientRole,
    /// What it can handle. A capability that is not stated is not used.
    pub capabilities: ClientCapabilities,
    /// How many events may wait for it before its deltas are dropped.
    pub queue: usize,
}

impl ClientInfo {
    /// A person's front end that answers approvals and wants deltas: what
    /// the terminal UI is.
    #[must_use]
    pub fn user() -> Self {
        Self {
            role: ClientRole::User,
            capabilities: ClientCapabilities {
                approvals: true,
                questions: true,
                trust: true,
                deltas: true,
                experimental: Vec::new(),
            },
            queue: crate::events::QUEUE_CAPACITY,
        }
    }

    /// A front end that answers nothing and wants whole items only: what
    /// headless mode is. With no other client, a call that needs an answer
    /// is denied.
    #[must_use]
    pub fn headless() -> Self {
        Self {
            role: ClientRole::User,
            capabilities: ClientCapabilities::default(),
            queue: crate::events::QUEUE_CAPACITY,
        }
    }

    /// Something that only watches.
    #[must_use]
    pub fn observer() -> Self {
        Self {
            role: ClientRole::Observer,
            capabilities: ClientCapabilities::default(),
            queue: crate::events::QUEUE_CAPACITY,
        }
    }
}

#[derive(Debug)]
struct Entry {
    info: ClientInfo,
    queue: Arc<EventQueue>,
}

/// A question that is open.
struct Open {
    session: SessionId,
    /// The clients it was put to.
    asked: Vec<u64>,
    answer: oneshot::Sender<ApprovalResponse>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each change to these tables is whole before the lock is let go.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The connected clients and the open requests.
#[derive(Default)]
pub(crate) struct Clients {
    next_client: AtomicU64,
    next_request: AtomicU64,
    clients: Mutex<BTreeMap<u64, Entry>>,
    open: Mutex<BTreeMap<RequestId, Open>>,
}

impl std::fmt::Debug for Clients {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clients")
            .field("clients", &lock(&self.clients).len())
            .field("open", &lock(&self.open).len())
            .finish()
    }
}

fn refused(code: &str, message: impl Into<String>) -> Box<ErrorData> {
    Box::new(ErrorData::new(ErrorKind::Usage, code, message))
}

impl Clients {
    /// Adds a client, and returns its number and its queue.
    pub(crate) fn connect(&self, info: ClientInfo) -> (ClientId, Arc<EventQueue>) {
        let id = self.next_client.fetch_add(1, Ordering::Relaxed) + 1;
        let queue = Arc::new(EventQueue::new(info.queue, info.capabilities.deltas));
        lock(&self.clients).insert(
            id,
            Entry {
                info,
                queue: Arc::clone(&queue),
            },
        );
        (ClientId(id), queue)
    }

    /// Removes a client. Its queue takes nothing more.
    pub(crate) fn disconnect(&self, client: ClientId) {
        if let Some(entry) = lock(&self.clients).remove(&client.0) {
            entry.queue.close();
        }
    }

    pub(crate) fn queue(&self, client: ClientId) -> Option<Arc<EventQueue>> {
        lock(&self.clients)
            .get(&client.0)
            .map(|entry| Arc::clone(&entry.queue))
    }

    pub(crate) fn info(&self, client: ClientId) -> Option<ClientInfo> {
        lock(&self.clients)
            .get(&client.0)
            .map(|entry| entry.info.clone())
    }

    /// Which of `candidates` may be asked whether a tool call may run: the
    /// `user` clients that said they answer approvals.
    pub(crate) fn approvers(&self, candidates: &[u64]) -> Vec<u64> {
        let clients = lock(&self.clients);
        candidates
            .iter()
            .copied()
            .filter(|client| {
                clients.get(client).is_some_and(|entry| {
                    entry.info.role == ClientRole::User && entry.info.capabilities.approvals
                })
            })
            .collect()
    }

    /// Puts a request to `asked`, and returns its id and where the answer
    /// will arrive.
    pub(crate) fn ask(
        &self,
        session: &SessionId,
        asked: &[u64],
        request: impl Fn(&RequestId) -> ServerRequest,
    ) -> (RequestId, oneshot::Receiver<ApprovalResponse>) {
        let number = self.next_request.fetch_add(1, Ordering::Relaxed) + 1;
        let id = RequestId::Number(i64::try_from(number).unwrap_or(i64::MAX));
        let (answer, receiver) = oneshot::channel();
        // The request is open before any client can see it, so that an
        // answer cannot arrive for a request nobody knows.
        lock(&self.open).insert(
            id.clone(),
            Open {
                session: session.clone(),
                asked: asked.to_vec(),
                answer,
            },
        );
        let clients = lock(&self.clients);
        for client in asked {
            if let Some(entry) = clients.get(client) {
                entry.queue.push_request(id.clone(), request(&id));
            }
        }
        (id, receiver)
    }

    /// Closes a request that will not be answered through here any more:
    /// it timed out, or was withdrawn. Says whether it was open.
    pub(crate) fn close(&self, id: &RequestId) -> bool {
        lock(&self.open).remove(id).is_some()
    }

    /// Takes a client's answer to an approval request.
    ///
    /// # Errors
    ///
    /// `role_not_allowed` when the client is not a `user` client or was not
    /// asked, and an error of kind `usage` when no such request is open: it
    /// was answered by another client, timed out or was withdrawn.
    pub(crate) fn answer(
        &self,
        client: ClientId,
        id: &RequestId,
        response: ApprovalResponse,
    ) -> Result<SessionId, Box<ErrorData>> {
        let role = self.info(client).map(|info| info.role);
        if role != Some(ClientRole::User) {
            return Err(refused(
                code::ROLE_NOT_ALLOWED,
                "Only a `user` client may answer an approval request.",
            ));
        }
        let mut open = lock(&self.open);
        let Some(request) = open.get(id) else {
            return Err(refused(
                "request_not_open",
                format!(
                    "No request `{id}` is waiting for an answer: it was answered already, timed \
                     out, or was withdrawn."
                ),
            ));
        };
        if !request.asked.contains(&client.0) {
            return Err(refused(
                code::ROLE_NOT_ALLOWED,
                format!("The request `{id}` was not put to this client."),
            ));
        }
        let Some(request) = open.remove(id) else {
            return Err(refused("request_not_open", "The request is gone."));
        };
        let session = request.session.clone();
        // If nobody waits for the answer any more, the question was
        // withdrawn in this instant, and the answer changes nothing.
        let _ = request.answer.send(response);
        Ok(session)
    }
}
