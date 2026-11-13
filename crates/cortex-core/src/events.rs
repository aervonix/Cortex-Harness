//! Where a session's events go: one bounded queue for each client, which
//! merges deltas and never makes the loop wait
//! (`docs/architecture/02-runtime.md`, "Event backpressure").
//!
//! The loop hands every event to the session's [`SessionSink`], which
//! gives a copy to the queue of each client that has the session open. A
//! queue takes an event without waiting, whatever its reader is doing:
//!
//! 1. **Deltas of one item are merged** when the reader is behind: a delta
//!    that follows a delta of the same kind for the same item, with nothing
//!    queued between them, is added to it. The reader sees fewer, larger
//!    deltas.
//! 2. **When the queue is full, deltas of that item are dropped**, and one
//!    `item.resync` is queued when there is room. From then until the item
//!    completes, the reader gets none of its deltas: it reads what the item
//!    has so far with `session/read`, and `item.completed` has all of it.
//! 3. **Nothing else is ever dropped.** A completed item, a turn's events
//!    and a request to the client are queued even when the queue is full:
//!    they are few, and each is bounded. (A transport that cannot hold them
//!    closes the connection; that is the RPC server's rule, in its step.)
//!
//! A client that did not ask for deltas (`capabilities.deltas`) is sent
//! none.
//!
//! The sink also keeps **what each item in progress holds so far**
//! ([`InProgress`]), built from the same deltas, which is what
//! `session/read` returns for an item that has not completed.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_agent::EventSink;
use cortex_protocol::event::{Delta, Event, ItemDelta, ItemKind, ItemRef, TextDelta};
use cortex_protocol::item::{AssistantMessageItem, ToolOutcome, ToolResultItem};
use cortex_protocol::jsonrpc::RequestId;
use cortex_protocol::message::{ReasoningPart, ToolCallPart};
use cortex_protocol::methods::ServerRequest;
use cortex_protocol::{CallId, Item, ItemId, Part, SessionEvent, SessionId, TurnId};
use serde_json::Value;
use tokio::sync::Notify;

/// How many events may wait for one client before its deltas are dropped.
pub const QUEUE_CAPACITY: usize = 1024;

/// What the outcome of a tool result in progress is called: the call is
/// still running.
pub const RUNNING: &str = "running";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every change to a queue or a tracker is complete before its lock is
    // let go.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One thing the core sends a client.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "nearly everything sent is an event, so a box would be an allocation per event"
)]
pub enum Outgoing {
    /// An event of a session the client has open.
    Event(SessionEvent),
    /// A request the client is to answer with
    /// [`Core::respond`](crate::Core::respond): an approval.
    Request {
        /// The request's id, which the answer names.
        id: RequestId,
        /// The request.
        request: ServerRequest,
    },
}

#[derive(Debug, Default)]
struct QueueState {
    items: VecDeque<Outgoing>,
    /// The items whose deltas are being dropped, each with the resync it is
    /// owed, until that is queued.
    dropping: BTreeMap<ItemId, Option<SessionEvent>>,
    closed: bool,
}

/// One client's queue of events and requests.
#[derive(Debug)]
pub struct EventQueue {
    state: Mutex<QueueState>,
    wake: Notify,
    capacity: usize,
    deltas: bool,
}

impl EventQueue {
    /// A queue that holds `capacity` events before it drops deltas, for a
    /// client that does or does not want deltas at all.
    #[must_use]
    pub fn new(capacity: usize, deltas: bool) -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            wake: Notify::new(),
            capacity: capacity.max(1),
            deltas,
        }
    }

    /// Takes an event. Never waits.
    pub fn push(&self, event: SessionEvent) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        match &event.event {
            Event::ItemDelta(delta) => {
                if !self.deltas || state.dropping.contains_key(&delta.item_id) {
                    self.settle_resyncs(&mut state);
                    return;
                }
                if let Some(Outgoing::Event(last)) = state.items.back_mut()
                    && merge(last, &event)
                {
                    return;
                }
                if state.items.len() >= self.capacity {
                    // The reader is far behind: this item's deltas stop
                    // here, and it is told once, when there is room.
                    let resync = SessionEvent {
                        session_id: event.session_id.clone(),
                        seq: event.seq,
                        turn_id: event.turn_id.clone(),
                        event: Event::ItemResync(ItemRef {
                            item_id: delta.item_id.clone(),
                        }),
                    };
                    state.dropping.insert(delta.item_id.clone(), Some(resync));
                    return;
                }
            }
            Event::ItemCompleted(completed) => {
                // The whole item follows: nothing is owed for it any more.
                if let Some(id) = completed.item.id() {
                    state.dropping.remove(&ItemId::new(id));
                }
            }
            Event::ItemReset(reset) => {
                state.dropping.remove(&reset.item_id);
            }
            _ => {}
        }
        self.settle_resyncs(&mut state);
        state.items.push_back(Outgoing::Event(event));
        drop(state);
        self.wake.notify_one();
    }

    /// Queues a request to the client. Never dropped.
    pub(crate) fn push_request(&self, id: RequestId, request: ServerRequest) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        state.items.push_back(Outgoing::Request { id, request });
        drop(state);
        self.wake.notify_one();
    }

    /// Queues the resyncs that are owed, while there is room.
    fn settle_resyncs(&self, state: &mut QueueState) {
        if state.items.len() >= self.capacity {
            return;
        }
        let QueueState {
            items, dropping, ..
        } = state;
        for owed in dropping.values_mut() {
            if items.len() >= self.capacity {
                break;
            }
            if let Some(resync) = owed.take() {
                items.push_back(Outgoing::Event(resync));
            }
        }
    }

    /// The next thing for the client, if there is one. Never waits.
    pub fn try_next(&self) -> Option<Outgoing> {
        let mut state = lock(&self.state);
        let next = state.items.pop_front();
        if next.is_some() {
            // Taking one makes room.
            self.settle_resyncs(&mut state);
        } else if !state.dropping.is_empty() {
            self.settle_resyncs(&mut state);
            return state.items.pop_front();
        }
        next
    }

    /// The next thing for the client. `None` once the client is
    /// disconnected and its queue is empty.
    pub async fn next(&self) -> Option<Outgoing> {
        loop {
            // Asked for before the queue is looked at, so that a push
            // between the look and the wait is not missed.
            let notified = self.wake.notified();
            if let Some(next) = self.try_next() {
                return Some(next);
            }
            if lock(&self.state).closed {
                return None;
            }
            notified.await;
        }
    }

    /// How many events and requests are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.state).items.len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The client is gone: what is queued can still be read, and nothing
    /// more is taken.
    pub(crate) fn close(&self) {
        lock(&self.state).closed = true;
        self.wake.notify_waiters();
        self.wake.notify_one();
    }
}

/// Adds `next` to `last` when both are deltas of the same kind for the
/// same item. The merged delta has the later `seq`.
fn merge(last: &mut SessionEvent, next: &SessionEvent) -> bool {
    if last.session_id != next.session_id || last.turn_id != next.turn_id {
        return false;
    }
    let (Event::ItemDelta(queued), Event::ItemDelta(new)) = (&mut last.event, &next.event) else {
        return false;
    };
    if queued.item_id != new.item_id {
        return false;
    }
    let merged = match (&mut queued.delta, &new.delta) {
        (Delta::Text(queued), Delta::Text(new))
        | (Delta::Reasoning(queued), Delta::Reasoning(new))
        | (Delta::ToolOutput(queued), Delta::ToolOutput(new)) => {
            queued.text.push_str(&new.text);
            true
        }
        (Delta::ToolArgs(queued), Delta::ToolArgs(new)) if queued.call_id == new.call_id => {
            queued.json.push_str(&new.json);
            true
        }
        _ => false,
    };
    if merged {
        last.seq = next.seq;
    }
    merged
}

/// What an item in progress holds so far.
#[derive(Debug, Clone)]
struct Partial {
    kind: ItemKind,
    tool: Option<String>,
    call_id: Option<CallId>,
    reasoning: String,
    text: String,
    output: String,
    /// The tool calls the model has begun, with what has arrived of their
    /// arguments.
    calls: Vec<(CallId, String, String)>,
}

/// The items of a session that have begun and not completed, with their
/// content so far.
#[derive(Debug, Default)]
pub(crate) struct InProgress {
    items: Vec<(ItemId, Partial)>,
}

impl InProgress {
    fn observe(&mut self, event: &Event) {
        match event {
            Event::ItemStarted(started) => {
                self.items.retain(|(id, _)| *id != started.item_id);
                self.items.push((
                    started.item_id.clone(),
                    Partial {
                        kind: started.kind.clone(),
                        tool: started.tool.clone(),
                        call_id: started.call_id.clone(),
                        reasoning: String::new(),
                        text: String::new(),
                        output: String::new(),
                        calls: Vec::new(),
                    },
                ));
            }
            Event::ItemDelta(ItemDelta { item_id, delta }) => {
                let Some((_, partial)) = self.items.iter_mut().find(|(id, _)| id == item_id) else {
                    return;
                };
                match delta {
                    Delta::Text(TextDelta { text }) => partial.text.push_str(text),
                    Delta::Reasoning(TextDelta { text }) => partial.reasoning.push_str(text),
                    Delta::ToolOutput(TextDelta { text }) => partial.output.push_str(text),
                    Delta::ToolCall(call) => {
                        partial.calls.push((
                            call.call_id.clone(),
                            call.name.clone(),
                            String::new(),
                        ));
                    }
                    Delta::ToolArgs(arguments) => {
                        if let Some((_, _, json)) = partial
                            .calls
                            .iter_mut()
                            .find(|(id, _, _)| *id == arguments.call_id)
                        {
                            json.push_str(&arguments.json);
                        }
                    }
                    Delta::Unknown(_) => {}
                }
            }
            Event::ItemReset(reset) => {
                // The partial content was discarded: the item begins again.
                if let Some((_, partial)) =
                    self.items.iter_mut().find(|(id, _)| *id == reset.item_id)
                {
                    partial.reasoning.clear();
                    partial.text.clear();
                    partial.output.clear();
                    partial.calls.clear();
                }
            }
            Event::ItemCompleted(completed) => {
                if let Some(done) = completed.item.id() {
                    self.items.retain(|(id, _)| id.as_str() != done);
                }
            }
            // A turn that is over has nothing in progress: an item that
            // began and never completed (a response thrown away) goes with
            // it.
            Event::TurnCompleted(_) => self.items.clear(),
            _ => {}
        }
    }

    /// The items in progress, oldest first, each with what it has so far.
    fn items(&self) -> Vec<Item> {
        self.items
            .iter()
            .filter_map(|(id, partial)| partial.item(id))
            .collect()
    }
}

impl Partial {
    fn item(&self, id: &ItemId) -> Option<Item> {
        match &self.kind {
            ItemKind::AssistantMessage => {
                let mut parts = Vec::new();
                if !self.reasoning.is_empty() {
                    parts.push(Part::Reasoning(ReasoningPart {
                        text: self.reasoning.clone(),
                        raw: None,
                    }));
                }
                if !self.text.is_empty() {
                    parts.push(Part::text(self.text.clone()));
                }
                for (call, name, json) in &self.calls {
                    parts.push(Part::ToolCall(ToolCallPart {
                        id: call.clone(),
                        name: name.clone(),
                        // Not valid JSON until the call is complete: the
                        // text so far, as text.
                        arguments: Value::String(json.clone()),
                        raw: None,
                    }));
                }
                Some(Item::AssistantMessage(AssistantMessageItem {
                    id: id.clone(),
                    parts,
                    stop_reason: None,
                    raw_stop: None,
                    stop_detail: None,
                    truncated: false,
                    origin: None,
                    usage: None,
                    error: None,
                    created_at: None,
                }))
            }
            ItemKind::ToolResult => Some(Item::ToolResult(ToolResultItem {
                id: id.clone(),
                call_id: self.call_id.clone()?,
                tool: self.tool.clone().unwrap_or_default(),
                outcome: ToolOutcome::from_wire(RUNNING),
                content: if self.output.is_empty() {
                    Vec::new()
                } else {
                    vec![Part::text(self.output.clone())]
                },
                payload: None,
                artifacts: Vec::new(),
                duration_ms: None,
                created_at: None,
            })),
            // Nothing else streams.
            _ => None,
        }
    }
}

/// The sink of one session: every client that has the session open gets
/// each event, and the items in progress are kept current.
#[derive(Debug, Default)]
pub(crate) struct SessionSink {
    subscribers: Mutex<Vec<(u64, Arc<EventQueue>)>>,
    progress: Mutex<InProgress>,
    /// The turn that is running and the `seq` of the last event, for a
    /// reader that asks where the session is.
    last: Mutex<(u64, Option<TurnId>)>,
}

impl SessionSink {
    /// Adds a client's queue, under the client's number. A client is added
    /// once.
    pub(crate) fn subscribe(&self, client: u64, queue: Arc<EventQueue>) {
        let mut subscribers = lock(&self.subscribers);
        if !subscribers.iter().any(|(known, _)| *known == client) {
            subscribers.push((client, queue));
        }
    }

    /// Removes a client's queue. Says whether it was there.
    pub(crate) fn unsubscribe(&self, client: u64) -> bool {
        let mut subscribers = lock(&self.subscribers);
        let before = subscribers.len();
        subscribers.retain(|(known, _)| *known != client);
        subscribers.len() != before
    }

    /// The clients that have the session open.
    pub(crate) fn subscribers(&self) -> Vec<u64> {
        lock(&self.subscribers)
            .iter()
            .map(|(client, _)| *client)
            .collect()
    }

    /// The items in progress, with what each has so far.
    pub(crate) fn in_progress(&self) -> Vec<Item> {
        lock(&self.progress).items()
    }

    /// The `seq` of the last event the session sent.
    pub(crate) fn seq(&self) -> u64 {
        lock(&self.last).0
    }
}

impl EventSink for SessionSink {
    fn emit(&self, event: SessionEvent) {
        lock(&self.progress).observe(&event.event);
        *lock(&self.last) = (event.seq, event.turn_id.clone());
        let subscribers = lock(&self.subscribers);
        if let Some(((_, last), rest)) = subscribers.split_last() {
            for (_, queue) in rest {
                queue.push(event.clone());
            }
            last.push(event);
        }
    }
}

/// The session a request or an event is about, for whoever routes it.
#[must_use]
pub fn session_of(outgoing: &Outgoing) -> Option<&SessionId> {
    match outgoing {
        Outgoing::Event(event) => Some(&event.session_id),
        Outgoing::Request { request, .. } => match request {
            ServerRequest::Approval(approval) => Some(&approval.session_id),
            ServerRequest::Question(question) => Some(&question.session_id),
            ServerRequest::Trust(trust) => Some(&trust.session_id),
        },
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::event::{
        ItemCompleted, ItemStarted, ToolArgsDelta, ToolCallDelta, TurnStarted,
    };
    use cortex_protocol::item::NoticeItem;
    use cortex_protocol::item::NoticeSource;

    use super::*;

    fn event(seq: u64, event: Event) -> SessionEvent {
        SessionEvent {
            session_id: SessionId::new("s_1"),
            seq,
            turn_id: Some(TurnId::new("t_1")),
            event,
        }
    }

    fn delta(seq: u64, item: &str, delta: Delta) -> SessionEvent {
        event(
            seq,
            Event::ItemDelta(ItemDelta {
                item_id: ItemId::new(item),
                delta,
            }),
        )
    }

    fn text(seq: u64, item: &str, text: &str) -> SessionEvent {
        delta(
            seq,
            item,
            Delta::Text(TextDelta {
                text: text.to_owned(),
            }),
        )
    }

    fn completed(seq: u64, item: &str) -> SessionEvent {
        event(
            seq,
            Event::ItemCompleted(ItemCompleted {
                item: Item::Notice(NoticeItem {
                    id: ItemId::new(item),
                    source: NoticeSource::Harness,
                    text: "done".to_owned(),
                    created_at: None,
                }),
            }),
        )
    }

    fn drain(queue: &EventQueue) -> Vec<SessionEvent> {
        std::iter::from_fn(|| queue.try_next())
            .map(|outgoing| match outgoing {
                Outgoing::Event(event) => event,
                Outgoing::Request { .. } => panic!("a request"),
            })
            .collect()
    }

    fn label(event: &SessionEvent) -> String {
        match &event.event {
            Event::ItemDelta(delta) => match &delta.delta {
                Delta::Text(text) => format!("text:{}:{}", delta.item_id, text.text),
                Delta::Reasoning(text) => format!("reasoning:{}:{}", delta.item_id, text.text),
                Delta::ToolOutput(text) => format!("output:{}:{}", delta.item_id, text.text),
                Delta::ToolArgs(arguments) => format!("args:{}:{}", delta.item_id, arguments.json),
                Delta::ToolCall(call) => format!("call:{}:{}", delta.item_id, call.name),
                Delta::Unknown(_) => "unknown".to_owned(),
            },
            Event::ItemResync(resync) => format!("resync:{}", resync.item_id),
            Event::ItemCompleted(done) => format!("completed:{}", done.item.id().unwrap_or("")),
            Event::TurnStarted(_) => "turn.started".to_owned(),
            other => format!("{other:?}"),
        }
    }

    fn labels(queue: &EventQueue) -> Vec<String> {
        drain(queue).iter().map(label).collect()
    }

    #[test]
    fn deltas_of_one_item_and_kind_are_merged_while_the_reader_is_behind() {
        let queue = EventQueue::new(16, true);
        queue.push(text(1, "i_1", "The "));
        queue.push(text(2, "i_1", "build "));
        queue.push(text(3, "i_1", "fails."));
        let events = drain(&queue);
        assert_eq!(events.len(), 1);
        assert_eq!(label(&events[0]), "text:i_1:The build fails.");
        // The merged delta is the last of them.
        assert_eq!(events[0].seq, 3);
        // A reader that keeps up sees each delta as it came.
        queue.push(text(4, "i_1", "a"));
        assert_eq!(labels(&queue), ["text:i_1:a"]);
        queue.push(text(5, "i_1", "b"));
        assert_eq!(labels(&queue), ["text:i_1:b"]);
    }

    #[test]
    fn only_neighbours_of_the_same_item_and_kind_merge() {
        let queue = EventQueue::new(16, true);
        let reasoning = |seq, text: &str| {
            delta(
                seq,
                "i_1",
                Delta::Reasoning(TextDelta {
                    text: text.to_owned(),
                }),
            )
        };
        let args = |seq, call: &str, json: &str| {
            delta(
                seq,
                "i_1",
                Delta::ToolArgs(ToolArgsDelta {
                    call_id: CallId::new(call),
                    json: json.to_owned(),
                }),
            )
        };
        queue.push(reasoning(1, "hm"));
        queue.push(reasoning(2, "m"));
        queue.push(text(3, "i_1", "a"));
        queue.push(text(4, "i_2", "b"));
        queue.push(text(5, "i_1", "c"));
        queue.push(delta(
            6,
            "i_1",
            Delta::ToolCall(ToolCallDelta {
                call_id: CallId::new("c1"),
                name: "read".to_owned(),
            }),
        ));
        queue.push(args(7, "c1", "{\"pa"));
        queue.push(args(8, "c1", "th\":1}"));
        queue.push(args(9, "c2", "{}"));
        // Something that is not a delta stands between two that would
        // otherwise merge: order is kept.
        queue.push(text(10, "i_1", "d"));
        queue.push(event(11, Event::TurnStarted(TurnStarted {})));
        queue.push(text(12, "i_1", "e"));
        assert_eq!(
            labels(&queue),
            [
                "reasoning:i_1:hmm",
                "text:i_1:a",
                "text:i_2:b",
                "text:i_1:c",
                "call:i_1:read",
                "args:i_1:{\"path\":1}",
                "args:i_1:{}",
                "text:i_1:d",
                "turn.started",
                "text:i_1:e",
            ]
        );
    }

    #[test]
    fn a_full_queue_drops_an_items_deltas_and_owes_it_one_resync() {
        let queue = EventQueue::new(3, true);
        queue.push(event(1, Event::TurnStarted(TurnStarted {})));
        queue.push(text(2, "i_1", "a"));
        queue.push(event(3, Event::TurnStarted(TurnStarted {})));
        // Full. This one cannot merge, and is dropped with all that follow
        // for the item.
        queue.push(text(4, "i_1", "b"));
        queue.push(text(5, "i_1", "c"));
        // Another item is dropped on its own account.
        queue.push(text(6, "i_2", "x"));
        assert_eq!(queue.len(), 3);
        // Taking one makes room for one resync; taking another, for the
        // second.
        assert_eq!(label(&drain_one(&queue)), "turn.started");
        assert_eq!(queue.len(), 3);
        // The item's deltas stay dropped after its resync is queued.
        queue.push(text(7, "i_1", "d"));
        assert_eq!(
            labels(&queue),
            ["text:i_1:a", "turn.started", "resync:i_1", "resync:i_2"]
        );
        queue.push(text(8, "i_1", "e"));
        assert!(queue.is_empty(), "dropped until the item completes");
        // Its completion is never dropped, and its deltas flow again after
        // (a new item with the same id cannot be, but the state is clean).
        queue.push(completed(9, "i_1"));
        queue.push(text(10, "i_1", "after"));
        assert_eq!(labels(&queue), ["completed:i_1", "text:i_1:after"]);
    }

    fn drain_one(queue: &EventQueue) -> SessionEvent {
        match queue.try_next() {
            Some(Outgoing::Event(event)) => event,
            other => panic!("no event: {other:?}"),
        }
    }

    #[test]
    fn an_item_that_completes_while_its_resync_is_owed_gets_no_resync() {
        let queue = EventQueue::new(1, true);
        queue.push(event(1, Event::TurnStarted(TurnStarted {})));
        queue.push(text(2, "i_1", "a"));
        queue.push(completed(3, "i_1"));
        // The completed item is queued although the queue was full.
        assert_eq!(labels(&queue), ["turn.started", "completed:i_1"]);
    }

    #[test]
    fn whole_events_and_requests_are_never_dropped() {
        let queue = EventQueue::new(2, true);
        for seq in 1..=50 {
            queue.push(completed(seq, &format!("i_{seq}")));
        }
        assert_eq!(queue.len(), 50);
        let seqs: Vec<u64> = drain(&queue).iter().map(|event| event.seq).collect();
        assert_eq!(seqs, (1..=50).collect::<Vec<u64>>());
    }

    #[test]
    fn a_client_that_did_not_ask_for_deltas_gets_none() {
        let queue = EventQueue::new(16, false);
        queue.push(text(1, "i_1", "a"));
        queue.push(completed(2, "i_1"));
        assert_eq!(labels(&queue), ["completed:i_1"]);
    }

    #[test]
    fn a_closed_queue_takes_nothing_more_and_ends_when_it_is_empty() {
        let queue = EventQueue::new(16, true);
        queue.push(completed(1, "i_1"));
        queue.close();
        queue.push(completed(2, "i_2"));
        let next = cortex_testkit::block_on(queue.next());
        assert!(matches!(next, Some(Outgoing::Event(event)) if event.seq == 1));
        assert_eq!(cortex_testkit::block_on(queue.next()), None);
    }

    #[test]
    fn the_sink_gives_every_subscriber_each_event_and_tracks_what_is_in_progress() {
        let sink = SessionSink::default();
        let (one, two) = (
            Arc::new(EventQueue::new(16, true)),
            Arc::new(EventQueue::new(16, false)),
        );
        sink.subscribe(1, Arc::clone(&one));
        sink.subscribe(2, Arc::clone(&two));
        sink.subscribe(1, Arc::clone(&one));
        assert_eq!(sink.subscribers(), [1, 2]);

        let started = |seq, item: &str, kind: ItemKind, tool: Option<&str>| {
            event(
                seq,
                Event::ItemStarted(ItemStarted {
                    item_id: ItemId::new(item),
                    kind,
                    tool: tool.map(str::to_owned),
                    call_id: tool.map(|_| CallId::new("c1")),
                }),
            )
        };
        sink.emit(started(1, "i_1", ItemKind::AssistantMessage, None));
        sink.emit(delta(
            2,
            "i_1",
            Delta::Reasoning(TextDelta {
                text: "thinking".to_owned(),
            }),
        ));
        sink.emit(text(3, "i_1", "Reading "));
        sink.emit(text(4, "i_1", "it."));
        sink.emit(delta(
            5,
            "i_1",
            Delta::ToolCall(ToolCallDelta {
                call_id: CallId::new("c1"),
                name: "read".to_owned(),
            }),
        ));
        sink.emit(delta(
            6,
            "i_1",
            Delta::ToolArgs(ToolArgsDelta {
                call_id: CallId::new("c1"),
                json: "{\"path\":".to_owned(),
            }),
        ));
        sink.emit(started(7, "i_2", ItemKind::ToolResult, Some("shell")));
        sink.emit(delta(
            8,
            "i_2",
            Delta::ToolOutput(TextDelta {
                text: "compiling\n".to_owned(),
            }),
        ));
        assert_eq!(sink.seq(), 8);

        let items = sink.in_progress();
        assert_eq!(items.len(), 2);
        let Item::AssistantMessage(message) = &items[0] else {
            panic!("not an assistant message");
        };
        assert_eq!(message.stop_reason, None, "still streaming");
        assert!(matches!(&message.parts[0], Part::Reasoning(part) if part.text == "thinking"));
        assert!(matches!(&message.parts[1], Part::Text(part) if part.text == "Reading it."));
        assert!(matches!(
            &message.parts[2],
            Part::ToolCall(call)
                if call.name == "read" && call.arguments == Value::String("{\"path\":".to_owned())
        ));
        let Item::ToolResult(result) = &items[1] else {
            panic!("not a tool result");
        };
        assert_eq!(result.outcome.as_str(), RUNNING);
        assert_eq!(result.tool, "shell");
        assert!(matches!(&result.content[0], Part::Text(part) if part.text == "compiling\n"));

        // A reset clears what the item had, and its completion ends it.
        sink.emit(event(
            9,
            Event::ItemReset(ItemRef {
                item_id: ItemId::new("i_1"),
            }),
        ));
        let Item::AssistantMessage(message) = &sink.in_progress()[0] else {
            panic!("not an assistant message");
        };
        assert!(message.parts.is_empty());
        sink.emit(completed(10, "i_2"));
        assert_eq!(sink.in_progress().len(), 1);

        // The client that wanted deltas got them; the other got the whole
        // events only.
        assert_eq!(drain(&one).len(), 9, "merged where they could be");
        let whole: Vec<u64> = drain(&two).iter().map(|event| event.seq).collect();
        assert_eq!(whole, [1, 7, 9, 10]);

        assert!(sink.unsubscribe(2));
        assert!(!sink.unsubscribe(2));
        sink.emit(completed(11, "i_1"));
        assert!(two.is_empty());
        assert_eq!(one.len(), 1);
        assert!(sink.in_progress().is_empty());
    }
}
