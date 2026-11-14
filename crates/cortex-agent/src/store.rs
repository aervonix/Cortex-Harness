//! What the loop needs from a session store, and a store that keeps it all
//! in memory (`docs/architecture/07-sessions-and-context.md`, "The log").
//!
//! The loop does not know where a session is kept. It reads the session
//! once, when it starts, and from then on only appends: an item when the
//! item is complete, an attempt that was given up, and at the end of a turn
//! how the turn ended. `cortex-session` keeps the real log; the core adapts
//! it to [`SessionStore`].
//!
//! **A failed append is fatal for the turn.** Either the record is on the
//! log when `append` returns `Ok`, or neither the log nor the store's memory
//! has it. The loop then leaves the item out of the context too, and ends
//! the turn with the error.
//!
//! **Appends are not cancelled.** The loop never races one against the
//! turn's cancellation token (`docs/architecture/02-runtime.md`,
//! "Cancellation safety").

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_base::{Classified, ContentHash, ErrorKind, IdCounter};
use cortex_llm::ToolDefinition;
use cortex_protocol::event::TurnCompleted;
use cortex_protocol::item::SummaryItem;
use cortex_protocol::usage::RequestUsage;
use cortex_protocol::{ErrorData, Item, ItemId, ModelId, Part, ProviderId, TurnId};
use cortex_tools::BoxFuture;
use serde_json::Value;

/// The prefix of a turn's id: `t_1`, `t_2`.
pub(crate) const TURN_PREFIX: &str = "t_";
/// The prefix of an item's id: `i_1`, `i_2`.
pub(crate) const ITEM_PREFIX: &str = "i_";

/// The store could not read or write the session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the session store failed: {message}")]
pub struct StoreError {
    /// What went wrong, for a person.
    pub message: String,
}

impl StoreError {
    /// An error with this message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Classified for StoreError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Internal
    }

    fn code(&self) -> &str {
        "store_failed"
    }
}

/// A session as the loop finds it when it starts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Opened {
    /// The items of the active branch, oldest first. A store that resumes a
    /// session has already repaired it: every tool call in them has a
    /// result.
    pub items: Vec<Item>,
    /// The highest turn number in the log, on any branch. 0 for a new
    /// session.
    pub last_turn: u64,
    /// The highest item number in the log, on any branch. 0 for a new
    /// session.
    pub last_item: u64,
    /// The context epochs that began on the active branch, oldest first,
    /// each with the number of items before it.
    pub epochs: Vec<(usize, EpochStart)>,
    /// The state the last compaction summary on the branch carried across.
    pub carried: Option<Value>,
    /// Whether an entry on the branch is held otherwise than it was sent:
    /// the secret filter masked it, or it was stored as its projection
    /// (`docs/architecture/07-sessions-and-context.md`, "Secrets in the
    /// log").
    pub masked: bool,
}

/// Why a context epoch began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochReason {
    /// The session's first request.
    Start,
    /// A session read again whose prompt or tools are no longer what its
    /// epoch had: the harness changed them.
    Resume,
    /// The model, its provider or its reasoning level changed. The prompt
    /// and the tools stay as they were.
    Model,
    /// A compaction: a summary begins the history.
    Compaction,
}

impl EpochReason {
    /// The name the log writes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Resume => "resume",
            Self::Model => "model",
            Self::Compaction => "compaction",
        }
    }

    /// The reason a log names, or `None` for a name this build does not
    /// know.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "start" => Self::Start,
            "resume" => Self::Resume,
            "model" => Self::Model,
            "compaction" => Self::Compaction,
            _ => return None,
        })
    }
}

/// The start of a context epoch: what every request of the epoch begins
/// with (`docs/architecture/07-sessions-and-context.md`, "Context
/// epochs").
#[derive(Debug, Clone, PartialEq)]
pub struct EpochStart {
    /// The provider of the main role's model.
    pub provider: ProviderId,
    /// Its wire dialect.
    pub protocol: String,
    /// The model.
    pub model: ModelId,
    /// The reasoning level as it was set, if one was.
    pub effort: Option<String>,
    /// The system prompt.
    pub system: String,
    /// The tools, in the order they are declared.
    pub tools: Vec<ToolDefinition>,
    /// Why it began. `None` in an epoch a later build wrote with a reason
    /// this one does not know.
    pub reason: Option<EpochReason>,
    /// The hash of the system prompt without its instruction files. An
    /// epoch read again goes on as it was when this is what the harness
    /// would build now.
    pub base: Option<ContentHash>,
    /// The instruction files the prompt holds, by the name the model is
    /// shown, with the hash of what was read of each.
    pub instructions: Vec<(String, ContentHash)>,
    /// The log does not hold the prompt or the tools as they were sent:
    /// the secret filter masked something in them. Such an epoch is not
    /// gone on with when the session is read again. Set by the store when
    /// it reads the log; never written.
    pub altered: bool,
}

/// A compaction summary, and the state carried across it.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// The summary as the model is sent it, carried state included.
    pub item: SummaryItem,
    /// The carried state as data: `plan`, `files`, `last_user`,
    /// `artifacts`.
    pub carried: Value,
}

/// A request attempt that was given up: it failed and was sent again, or
/// its response was thrown away. Kept for debugging, never sent to a model.
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    /// The assistant item the attempt would have become.
    pub item_id: ItemId,
    /// Which attempt of the request it was, counted from 1.
    pub attempt: u32,
    /// Why it was given up.
    pub error: ErrorData,
    /// What had arrived of the response.
    pub partial: Vec<Part>,
}

/// One thing the loop appends to a session.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
#[expect(
    clippy::large_enum_variant,
    reason = "nearly every record is an item, so a box would be an allocation per record"
)]
pub enum Record {
    /// A completed item: a user message, an assistant message, a tool
    /// result or a notice.
    Item(Item),
    /// A request attempt that was given up.
    Attempt(Attempt),
    /// A context epoch begins.
    Epoch(EpochStart),
    /// A compaction summary begins an epoch's history.
    Summary(Summary),
    /// What a request outside the conversation used: a summary's.
    Usage {
        /// What the request was for: `compaction`.
        purpose: String,
        /// What it used and cost.
        usage: RequestUsage,
    },
}

/// Where a session is kept, as far as the loop needs to know.
///
/// The loop owns its store and waits for every call to finish before it
/// goes on, so an implementation needs no lock for the loop's sake. Every
/// method may take time (a flush to disk): an implementation over blocking
/// code runs it on the blocking pool.
pub trait SessionStore: Send {
    /// The session as it stands. Called once before the first turn, and
    /// again whenever the session is told to reload (after a rewind or a
    /// change of branch).
    ///
    /// **While a session is open only the loop makes turn and item ids,**
    /// continuing from the two numbers this returns.
    fn open(&mut self) -> BoxFuture<'_, Result<Opened, StoreError>>;

    /// Appends one record of `turn`. `Ok` means it is on the log.
    fn append<'a>(
        &'a mut self,
        turn: &'a TurnId,
        record: Record,
    ) -> BoxFuture<'a, Result<(), StoreError>>;

    /// The turn is over. Records how it ended, and is the place to flush
    /// for a store that flushes once a turn.
    fn end_turn<'a>(
        &'a mut self,
        turn: &'a TurnId,
        outcome: &'a TurnCompleted,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// A session kept in memory, for a session with no disk and for tests.
///
/// Clones are the same session, so whoever made the store can read what the
/// loop appended to it.
#[derive(Debug, Clone, Default)]
pub struct MemorySessionStore {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    records: Vec<(TurnId, Record)>,
    outcomes: Vec<(TurnId, TurnCompleted)>,
    last_turn: u64,
    last_item: u64,
    /// An item is held otherwise than it was sent ([`MemorySessionStore::mask`]).
    masked: bool,
}

/// What [`Inner::read`] finds.
struct Read {
    items: Vec<Item>,
    epochs: Vec<(usize, EpochStart)>,
    carried: Option<Value>,
}

impl MemorySessionStore {
    /// An empty session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A session that already holds these items, as one that is resumed
    /// does. The id counters continue after the highest ids among them.
    #[must_use]
    pub fn with_items(turn: &TurnId, items: impl IntoIterator<Item = Item>) -> Self {
        let store = Self::new();
        {
            let mut inner = store.lock();
            inner.note_turn(turn);
            for item in items {
                inner.note_item(&item);
                inner.records.push((turn.clone(), Record::Item(item)));
            }
        }
        store
    }

    /// Stands for the secret filter: the item with `item`'s id is held from
    /// now on as `item`, otherwise than it was sent, and the store says so
    /// when the session is opened ([`Opened::masked`]).
    pub fn mask(&self, item: Item) {
        let mut inner = self.lock();
        for (_, record) in &mut inner.records {
            if let Record::Item(held) = record
                && held.id() == item.id()
            {
                *held = item.clone();
            }
        }
        inner.masked = true;
    }

    /// Every record appended so far, with its turn, in order.
    #[must_use]
    pub fn records(&self) -> Vec<(TurnId, Record)> {
        self.lock().records.clone()
    }

    /// The items appended so far, in order.
    #[must_use]
    pub fn items(&self) -> Vec<Item> {
        self.lock().items()
    }

    /// The attempts that were given up, in order.
    #[must_use]
    pub fn attempts(&self) -> Vec<Attempt> {
        self.lock()
            .records
            .iter()
            .filter_map(|(_, record)| match record {
                Record::Attempt(attempt) => Some(attempt.clone()),
                _ => None,
            })
            .collect()
    }

    /// The context epochs that began, in order.
    #[must_use]
    pub fn epochs(&self) -> Vec<EpochStart> {
        self.lock()
            .records
            .iter()
            .filter_map(|(_, record)| match record {
                Record::Epoch(epoch) => Some(epoch.clone()),
                _ => None,
            })
            .collect()
    }

    /// The compaction summaries, in order.
    #[must_use]
    pub fn summaries(&self) -> Vec<Summary> {
        self.lock()
            .records
            .iter()
            .filter_map(|(_, record)| match record {
                Record::Summary(summary) => Some(summary.clone()),
                _ => None,
            })
            .collect()
    }

    /// What requests outside the conversation used, in order.
    #[must_use]
    pub fn out_of_band(&self) -> Vec<(String, RequestUsage)> {
        self.lock()
            .records
            .iter()
            .filter_map(|(_, record)| match record {
                Record::Usage { purpose, usage } => Some((purpose.clone(), usage.clone())),
                _ => None,
            })
            .collect()
    }

    /// How each turn ended, in order.
    #[must_use]
    pub fn outcomes(&self) -> Vec<(TurnId, TurnCompleted)> {
        self.lock().outcomes.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every change is one push or one number, so a panic elsewhere
        // while the lock was held cannot have left the session half-written.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inner {
    fn items(&self) -> Vec<Item> {
        self.read().items
    }

    fn read(&self) -> Read {
        let mut read = Read {
            items: Vec::new(),
            epochs: Vec::new(),
            carried: None,
        };
        for (_, record) in &self.records {
            match record {
                Record::Item(item) => read.items.push(item.clone()),
                Record::Summary(summary) => {
                    read.items.push(Item::Summary(summary.item.clone()));
                    read.carried = Some(summary.carried.clone());
                }
                Record::Epoch(epoch) => read.epochs.push((read.items.len(), epoch.clone())),
                _ => {}
            }
        }
        read
    }

    fn note_turn(&mut self, turn: &TurnId) {
        if let Some(number) = IdCounter::new(TURN_PREFIX).number_of(turn.as_str()) {
            self.last_turn = self.last_turn.max(number);
        }
    }

    fn note_item(&mut self, item: &Item) {
        let number = item
            .id()
            .and_then(|id| IdCounter::new(ITEM_PREFIX).number_of(id));
        if let Some(number) = number {
            self.last_item = self.last_item.max(number);
        }
    }
}

impl SessionStore for MemorySessionStore {
    fn open(&mut self) -> BoxFuture<'_, Result<Opened, StoreError>> {
        let inner = self.lock();
        let read = inner.read();
        let opened = Opened {
            items: read.items,
            last_turn: inner.last_turn,
            last_item: inner.last_item,
            epochs: read.epochs,
            carried: read.carried,
            masked: inner.masked,
        };
        Box::pin(std::future::ready(Ok(opened)))
    }

    fn append<'a>(
        &'a mut self,
        turn: &'a TurnId,
        record: Record,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        {
            let mut inner = self.lock();
            inner.note_turn(turn);
            match &record {
                Record::Item(item) => inner.note_item(item),
                Record::Summary(summary) => inner.note_item(&Item::Summary(summary.item.clone())),
                Record::Attempt(attempt) => {
                    let number = IdCounter::new(ITEM_PREFIX).number_of(attempt.item_id.as_str());
                    inner.last_item = inner.last_item.max(number.unwrap_or(0));
                }
                Record::Epoch(_) | Record::Usage { .. } => {}
            }
            inner.records.push((turn.clone(), record));
        }
        Box::pin(std::future::ready(Ok(())))
    }

    fn end_turn<'a>(
        &'a mut self,
        turn: &'a TurnId,
        outcome: &'a TurnCompleted,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        {
            let mut inner = self.lock();
            inner.note_turn(turn);
            inner.outcomes.push((turn.clone(), outcome.clone()));
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::event::TurnOutcome;
    use cortex_protocol::item::{NoticeItem, NoticeSource};

    use super::*;

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("the memory store never waits"),
        }
    }

    fn notice(id: &str) -> Item {
        Item::Notice(NoticeItem {
            id: ItemId::new(id),
            source: NoticeSource::Harness,
            text: "note".to_owned(),
            created_at: None,
        })
    }

    #[test]
    fn it_keeps_what_is_appended_and_says_where_the_counters_stand() {
        let mut store = MemorySessionStore::new();
        let reader = store.clone();
        assert_eq!(block_on(store.open()), Ok(Opened::default()));

        let turn = TurnId::new("t_3");
        block_on(store.append(&turn, Record::Item(notice("i_7")))).unwrap();
        let attempt = Attempt {
            item_id: ItemId::new("i_9"),
            attempt: 1,
            error: ErrorData::new(cortex_protocol::ErrorKind::Transient, "overloaded", "Busy."),
            partial: vec![Part::text("half")],
        };
        block_on(store.append(&turn, Record::Attempt(attempt.clone()))).unwrap();
        let ended = TurnCompleted {
            outcome: TurnOutcome::Completed,
            stop_reason: None,
            error: None,
            usage: cortex_protocol::usage::UsageTotals::default(),
            verification: None,
            duration_ms: None,
        };
        block_on(store.end_turn(&turn, &ended)).unwrap();

        let opened = block_on(store.open()).unwrap();
        assert_eq!(opened.items, [notice("i_7")]);
        assert!(opened.epochs.is_empty() && opened.carried.is_none());
        assert_eq!((opened.last_turn, opened.last_item), (3, 9));
        assert_eq!(reader.attempts(), [attempt]);
        assert_eq!(reader.outcomes(), [(turn, ended)]);
        assert_eq!(reader.records().len(), 2);
    }

    #[test]
    fn a_resumed_session_continues_after_its_highest_ids() {
        let mut store =
            MemorySessionStore::with_items(&TurnId::new("t_12"), [notice("i_40"), notice("i_38")]);
        let opened = block_on(store.open()).unwrap();
        assert_eq!(opened.items.len(), 2);
        assert_eq!((opened.last_turn, opened.last_item), (12, 40));
    }

    #[test]
    fn a_store_error_is_an_internal_failure() {
        let error = StoreError::new("disk full");
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert_eq!(error.code(), "store_failed");
        assert_eq!(error.to_string(), "the session store failed: disk full");
    }
}
