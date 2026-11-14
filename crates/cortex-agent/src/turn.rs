//! The loop itself: one session's turns, state by state
//! (`docs/architecture/03-agent-loop.md`, "Turn state machine").
//!
//! A [`Session`] is one task. It waits, idle, for a prompt; runs the turn
//! the prompt starts, as a walk through the states of
//! [`TurnState`](crate::TurnState); and when the turn completed and a
//! prompt is queued, runs the next. Each state is one method, which does
//! the state's work and returns where the turn goes next, with whatever
//! the next state needs. Nothing about a turn is kept in a flag.
//!
//! **Cancellation is an input.** Every wait that can take time is raced
//! against the turn's token. What must not be cut short (an append to the
//! log, recording the results of a round) is never raced.
//!
//! What the loop does for its context (epochs, instruction files, the
//! budget, compaction) is in [`epochs`].

mod epochs;

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use cortex_base::{AbsPath, CancelToken, Classified, IdCounter, SharedClock};
use cortex_llm::error::ProviderError;
use cortex_llm::{CooldownChange, ModelEvent, ModelRequest, SpeedCooldowns, SpeedMove, Usage};
use cortex_protocol::event::{
    Event, InfoNotice, ItemCompleted, ItemDelta, ItemKind, ItemRef, ItemStarted, LimitNotice,
    Notice, RetryNotice, SessionIdle, TurnCompleted, TurnOutcome, TurnStarted, UsageUpdated,
    Verification,
};
use cortex_protocol::item::{
    AssistantMessageItem, Delivery, NoticeItem, NoticeSource, UserMessageItem,
};
use cortex_protocol::message::Origin;
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::RoleState;
use cortex_protocol::speed::{CooldownState, SpeedFallbackBy};
use cortex_protocol::usage::{RequestUsage, UsageTotals};
use cortex_protocol::{ErrorData, ErrorKind, Item, ItemId, Part, StopReason, TurnId};
use tokio::sync::Notify;

use crate::assistant::{Applied, Calls, Draft};
use crate::config::{LoopConfig, Models, RoleModel, SessionInfo, Workspace};
use crate::context::{Context, ContextParts, Pressure};
use crate::events::{Emitter, EventSink};
use crate::guard::{Repetition, RepetitionGuard};
use crate::guarded::Guarded;
use crate::ledger::Ledger;
use crate::policy::Policy;
use crate::round::{Report, Round, RoundEnv, Stage};
use crate::session::{Inbox, Pending, Prompt, Queued, Running, SessionHandle, Shared};
use crate::speed::{Moves, SpeedSettings, cooldown_notice, fallback_notice};
use crate::state::{Observer, TurnState};
use crate::stats::{ContextStat, RequestStat, StatEvent, Stats, ToolCallStat, TurnStat};
use crate::store::{Attempt, ITEM_PREFIX, Record, SessionStore, StoreError, TURN_PREFIX};
use crate::toolset::ToolSet;
use crate::usage::{UsageBook, add_request};
use crate::verify::{Verifier, VerifyCx, VerifyOutcome};

/// Everything a session is made of.
pub struct SessionParts {
    /// What the loop is told about the session and does not change.
    pub info: SessionInfo,
    /// The models, by role.
    pub models: Models,
    /// The speed the session and its roles start at.
    pub speeds: SpeedSettings,
    /// The tools the model may call.
    pub tools: Arc<dyn ToolSet>,
    /// Decides whether a planned call may run.
    pub policy: Arc<dyn Policy>,
    /// Where the session is kept.
    pub store: Box<dyn SessionStore>,
    /// What the session's context is made from: the system prompt's
    /// facts, the instruction files, the thresholds.
    pub context: ContextParts,
    /// Where the session's events go.
    pub events: Arc<dyn EventSink>,
    /// Where its statistics events go.
    pub stats: Arc<dyn Stats>,
    /// Runs the project's checks.
    pub verifier: Arc<dyn Verifier>,
    /// The clock: timestamps, durations and the wall-clock limit.
    pub clock: SharedClock,
    /// The table of speed-tier cooldowns: the same one the model clients
    /// were given.
    pub cooldowns: SpeedCooldowns,
    /// The limits the session draws on.
    pub ledger: Ledger,
    /// Where the session's tools work.
    pub workspace: Workspace,
    /// How the loop is set.
    pub config: LoopConfig,
    /// The session's cancellation token: a child of the process's.
    pub cancel: CancelToken,
    /// Told of every change of state.
    pub observer: Option<Arc<dyn Observer>>,
}

impl std::fmt::Debug for SessionParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionParts")
            .field("info", &self.info)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// One session's loop. [`run`](Self::run) is the task; a [`SessionHandle`]
/// is how everything else reaches it.
pub struct Session {
    shared: Arc<Shared>,
    env: Arc<RoundEnv>,
    store: Box<dyn SessionStore>,
    context: Context,
    stats: Arc<dyn Stats>,
    verifier: Arc<dyn Verifier>,
    clock: SharedClock,
    ledger: Ledger,
    config: LoopConfig,
    observer: Option<Arc<dyn Observer>>,
    item_ids: IdCounter,
    /// The main model of the request that is being prepared or is in
    /// flight, and the version of the catalog its prices come from. It is
    /// read from the session's settings once for each request, in
    /// `Preparing`, so that a change of model in the middle of a request
    /// applies from the next one and a request is accounted to the model
    /// it was sent to.
    model: RoleModel,
    catalog_version: String,
    /// How many tool rounds have had their results recorded, and so sent
    /// to the model with the next request. A round's number is one more:
    /// it moves on only when the model will see what the round returned,
    /// which is what a tool takes a later round to mean.
    rounds_recorded: u64,
    state: TurnState,
    usage: UsageBook,
    moves: Moves,
    guard: RepetitionGuard,
    /// The roles' state as front ends were last told it.
    told_roles: Vec<RoleState>,
    /// When `usage.updated` was last sent, on the monotonic clock.
    usage_sent: Option<Duration>,
    /// A summary taken in the background, running or ready.
    background: Option<epochs::Background>,
    /// The epoch in which a summary in the background failed: no other is
    /// started in it.
    background_failed: Option<u64>,
    /// A compaction the user asked for is waiting.
    manual: bool,
    /// What it should keep in particular.
    manual_focus: Option<String>,
    /// The context record of the request in flight, sent with its
    /// `Request` event.
    pending_context: Option<ContextStat>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.shared.info.id)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// One turn, while it runs.
struct Turn {
    id: TurnId,
    /// The turn's number in the session, counted from 1.
    number: u32,
    cancel: CancelToken,
    /// The token of a second interrupt, which every tool call is given.
    kill: CancelToken,
    /// When it started, on the monotonic clock.
    started: Duration,
    usage: UsageTotals,
    requests: u64,
    tool_calls: u64,
    /// The files its tools have written, sorted.
    written: Vec<AbsPath>,
    verify_rounds: u32,
    verification: Option<Verification>,
    /// Why the last response ended.
    stop: Option<StopReason>,
}

/// What the request being prepared has already been through. It travels
/// with the request from state to state, and a new request starts clean.
#[derive(Debug, Clone, Copy, Default)]
struct Prep {
    /// The context was compacted for this request because it was over its
    /// budget.
    compacted: bool,
    /// The provider said the request did not fit, and a compaction answered
    /// that. A second time ends the turn.
    overflowed: bool,
    /// The response was cut off inside a tool call, and the request is
    /// sent once more with the model's whole output limit.
    raised_output: bool,
    /// The turn only compacts: `session/compact` on an idle session.
    only: bool,
}

/// The response as far as it has arrived.
struct Live {
    /// The id of the assistant item: one per request, whatever number of
    /// attempts it takes.
    item_id: ItemId,
    draft: Draft,
    /// Whether front ends were told the item began.
    shown: bool,
    /// Which attempt is arriving, counted from 1.
    attempt: u32,
    /// What the attempt has used so far, where the provider said.
    usage: Option<Usage>,
    /// When the request was sent, and when its first content arrived.
    sent: Duration,
    first_content: Option<Duration>,
}

/// A response that ended.
struct Response {
    live: Live,
    round: Round,
    stop: StopReason,
    raw_stop: String,
    stop_detail: Option<String>,
    usage: Usage,
    prep: Prep,
}

/// How a turn ends, when it is not interrupted.
struct End {
    outcome: TurnOutcome,
    error: Option<ErrorData>,
}

impl End {
    fn completed() -> Self {
        Self {
            outcome: TurnOutcome::Completed,
            error: None,
        }
    }

    fn failed(error: ErrorData) -> Self {
        Self {
            outcome: TurnOutcome::Failed,
            error: Some(error),
        }
    }

    fn store(error: &StoreError) -> Self {
        Self::failed(error.to_error_data())
    }
}

/// Where a turn goes next.
enum Phase {
    Preparing(Prep),
    Compacting(Prep),
    Requesting(Box<ModelRequest>, Prep),
    Deciding(Box<Response>),
    ToolRound(Box<Round>),
    Verifying,
    Finishing(End),
    /// With the response that was arriving, if one was.
    Interrupted(Option<Box<(Live, Round)>>),
}

/// What ended a wait while a response arrives.
enum Woke {
    Cancelled,
    Report(Report),
    Event(Option<ModelEvent>),
}

/// Waits for the turn to be cancelled, for a report from the round, or for
/// the next event of the response. Cancellation is looked at first, so
/// cancelled work does not go on; then the round, so that a call that could
/// start is not kept waiting by a response that has more to say.
async fn wake(
    cancel: &CancelToken,
    report: impl Future<Output = Report>,
    event: impl Future<Output = Option<ModelEvent>>,
) -> Woke {
    let mut cancelled = pin!(cancel.cancelled());
    let mut report = pin!(report);
    let mut event = pin!(event);
    std::future::poll_fn(|context| {
        if cancelled.as_mut().poll(context).is_ready() {
            return Poll::Ready(Woke::Cancelled);
        }
        if let Poll::Ready(report) = report.as_mut().poll(context) {
            return Poll::Ready(Woke::Report(report));
        }
        if let Poll::Ready(event) = event.as_mut().poll(context) {
            return Poll::Ready(Woke::Event(event));
        }
        Poll::Pending
    })
    .await
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl Session {
    /// Opens a session: reads it from its store, and returns its loop and a
    /// handle to it. Nothing runs until [`run`](Self::run) is polled.
    ///
    /// # Errors
    ///
    /// When the store cannot read the session.
    pub async fn open(parts: SessionParts) -> Result<(Self, SessionHandle), StoreError> {
        let SessionParts {
            info,
            models,
            speeds,
            tools,
            policy,
            mut store,
            context,
            events,
            stats,
            verifier,
            clock,
            cooldowns,
            ledger,
            workspace,
            config,
            cancel,
            observer,
        } = parts;
        let opened = store.open().await?;
        let mut context = Context::new(context, &tools.specs());
        context.load(&opened);

        let emitter = Arc::new(Emitter::new(info.id.clone(), events));
        let env = Arc::new(RoundEnv {
            session: info.id.clone(),
            tools,
            policy,
            workspace,
            emitter: Arc::clone(&emitter),
            clock: Arc::clone(&clock),
            max_parallel: config.max_parallel_tools.max(1),
        });
        let model = models.main().clone();
        let catalog_version = models.catalog_version().to_owned();
        let (level, lease_id, title, sandbox) = (
            info.level.clone(),
            info.lease_id.clone(),
            info.title.clone(),
            info.sandbox.clone(),
        );
        let shared = Arc::new(Shared {
            info,
            cooldowns,
            emitter,
            cancel,
            wake: Notify::new(),
            inbox: std::sync::Mutex::new(Inbox {
                turn_ids: IdCounter::new(TURN_PREFIX).after(opened.last_turn),
                running: None,
                starting: None,
                pending: std::collections::VecDeque::new(),
                queue: std::collections::VecDeque::new(),
                held: std::collections::VecDeque::new(),
                speeds,
                models,
                level,
                lease_id,
                sandbox,
                title,
                state: TurnState::Idle,
                usage: cortex_protocol::usage::SessionUsage::default(),
                reload: false,
                changing: false,
                history_changed: false,
                history: !opened.items.is_empty(),
                windows: std::collections::BTreeMap::new(),
                closed: false,
            }),
        });
        let told_roles = shared.state(&shared.lock()).roles.into_values().collect();
        let session = Self {
            shared: Arc::clone(&shared),
            env,
            store,
            context,
            stats,
            verifier,
            clock,
            ledger,
            guard: RepetitionGuard::new(config.repetition),
            config,
            observer,
            item_ids: IdCounter::new(ITEM_PREFIX).after(opened.last_item),
            model,
            catalog_version,
            rounds_recorded: 0,
            state: TurnState::Idle,
            usage: UsageBook::default(),
            moves: Moves::default(),
            told_roles,
            usage_sent: None,
            background: None,
            background_failed: None,
            manual: false,
            manual_focus: None,
            pending_context: None,
        };
        Ok((session, SessionHandle { shared }))
    }

    /// Runs the session as a task of its own on the current runtime.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.run())
    }

    /// The loop: runs until the session is closed or its token is
    /// cancelled, and a turn that was running then has ended.
    pub async fn run(mut self) {
        while let Some(start) = self.next_prompt().await {
            // The session is no longer idle. (When one turn follows another
            // front ends are told as the first one ends.)
            self.shared.emit_updated(&self.shared.lock());
            let mut next = Some(start);
            while let Some((running, prompt)) = next.take() {
                let turn = running.id.clone();
                // A panic is a bug. It ends the turn, and the session can
                // take the next prompt.
                let ended = Guarded(Box::pin(self.turn(running, prompt))).await;
                next = match ended {
                    Ok(next) => next,
                    Err(panic) => {
                        self.recover(&turn, &panic);
                        None
                    }
                };
            }
        }
    }

    /// Ends a turn whose code panicked: front ends get `turn.completed`
    /// with the failure, whatever was waiting is held, and the session is
    /// read again from its store before the next turn, because what the
    /// loop holds in memory can no longer be trusted.
    fn recover(&mut self, turn: &TurnId, panic: &str) {
        let completed = TurnCompleted {
            outcome: TurnOutcome::Failed,
            stop_reason: None,
            error: Some(ErrorData::new(
                ErrorKind::Internal,
                "loop_panicked",
                format!(
                    "internal error: the session's loop panicked ({panic}). The turn was ended."
                ),
            )),
            usage: UsageTotals::default(),
            verification: None,
            duration_ms: None,
        };
        self.shared
            .emitter
            .emit(Some(turn), Event::TurnCompleted(completed));
        self.state = TurnState::Idle;
        let mut inbox = self.shared.lock();
        Self::hold(&mut inbox);
        inbox.running = None;
        inbox.reload = true;
        inbox.state = TurnState::Idle;
        self.shared.emit_updated(&inbox);
    }

    // ------------------------------------------------------------------ idle

    /// Waits, with no timer, for a prompt to start a turn. `None` when the
    /// session is closed.
    async fn next_prompt(&mut self) -> Option<(Running, Prompt)> {
        loop {
            {
                let mut inbox = self.shared.lock();
                if let Some(prompt) = inbox.starting.take()
                    && let Some(running) = inbox.running.clone()
                {
                    return Some((running, prompt));
                }
                if inbox.closed || self.shared.cancel.is_cancelled() {
                    return None;
                }
            }
            // A wake that came since the look above is kept by the
            // notifier, so it is not missed.
            let woken = self.shared.cancel.race(self.shared.wake.notified()).await;
            if woken.is_none() && self.shared.lock().starting.is_none() {
                return None;
            }
        }
    }

    /// Reads the session again from its store: its items changed under the
    /// loop, or an append failed and memory must be brought back to what
    /// the log holds.
    ///
    /// **The table of seen files starts a new context epoch here**, for
    /// every reason the history is read again: what the model was shown
    /// may not be in the history any more (a rewind took it away), so the
    /// next read of any file is in full and never "unchanged" or a diff
    /// against a version the model does not have. After a change of history
    /// ([`HistoryChange::made`](crate::HistoryChange::made)) it forgets
    /// every file whole, snapshots too: one read or written only in a turn
    /// the rewind took away would otherwise pass the staleness guard and be
    /// overwritten unseen. This is the one place a change of history
    /// reaches the table, and where compaction starts its epochs too
    /// (`docs/architecture/07-sessions-and-context.md`, "What each part of
    /// a session's state follows").
    async fn reload(&mut self) -> Result<(), StoreError> {
        let changed = std::mem::take(&mut self.shared.lock().history_changed);
        self.new_file_epoch();
        if changed {
            self.env.workspace.files.forget_all();
        }
        // A summary taken over the history as it was is not of the branch
        // the loop reads now, and the request before is not the one the
        // next must begin with.
        self.drop_background();
        if changed {
            self.context.last_prefix = None;
        }
        let opened = self.store.open().await?;
        self.context.load(&opened);
        self.shared.lock().history = !opened.items.is_empty();
        let next = self.item_ids.peek().max(opened.last_item.saturating_add(1));
        self.item_ids = IdCounter::new(ITEM_PREFIX).after(next - 1);
        Ok(())
    }

    // ------------------------------------------------------------ the turn

    /// Runs one turn to its end. Returns the prompt of the next turn, when
    /// one starts by itself.
    async fn turn(&mut self, running: Running, prompt: Prompt) -> Option<(Running, Prompt)> {
        let number = IdCounter::new(TURN_PREFIX)
            .number_of(running.id.as_str())
            .and_then(|number| u32::try_from(number).ok())
            .unwrap_or(0);
        let mut turn = Turn {
            id: running.id,
            number,
            cancel: running.cancel,
            kill: running.kill,
            started: self.clock.monotonic(),
            usage: UsageTotals::default(),
            requests: 0,
            tool_calls: 0,
            written: Vec::new(),
            verify_rounds: 0,
            verification: None,
            stop: None,
        };
        self.ledger.begin_turn();
        self.guard.reset();
        self.enter(TurnState::Preparing);
        self.emit(&turn, Event::TurnStarted(TurnStarted {}));

        // If the session's history changed under the loop it is read again
        // first. When that fails the turn fails, and the next turn tries
        // again: no request goes out on a history that may have a hole.
        let reload = std::mem::take(&mut self.shared.lock().reload);
        let ready = if reload { self.reload().await } else { Ok(()) };
        let only = prompt.compact.is_some();
        let begun = match ready {
            Ok(()) => self.begin(&turn, prompt).await,
            Err(error) => Err(error),
        };
        let mut phase = match begun {
            Ok(()) if only => Phase::Compacting(Prep {
                only: true,
                ..Prep::default()
            }),
            Ok(()) => Phase::Preparing(Prep::default()),
            Err(error) => Phase::Finishing(End::store(&error)),
        };
        loop {
            phase = match phase {
                Phase::Preparing(prep) => self.prepare(&mut turn, prep).await,
                Phase::Compacting(prep) => self.compacting(&mut turn, prep).await,
                Phase::Requesting(request, prep) => self.respond(&mut turn, *request, prep).await,
                Phase::Deciding(response) => self.decide(&mut turn, *response).await,
                Phase::ToolRound(round) => self.tool_round(&mut turn, *round).await,
                Phase::Verifying => self.verify(&mut turn).await,
                Phase::Finishing(end) => return self.finish(&turn, end).await,
                Phase::Interrupted(partial) => {
                    self.interrupted(&mut turn, partial).await;
                    return None;
                }
            };
        }
    }

    /// The first thing a turn records: what its context needs (the epoch,
    /// changed instruction files, the date), the notices that were waiting
    /// for it, then the prompt. A turn that only compacts records no
    /// prompt.
    async fn begin(&mut self, turn: &Turn, prompt: Prompt) -> Result<(), StoreError> {
        self.context_turn_start(turn).await?;
        let notices: Vec<Pending> = {
            let mut inbox = self.shared.lock();
            let (notices, steering): (Vec<Pending>, Vec<Pending>) =
                std::mem::take(&mut inbox.pending)
                    .into_iter()
                    .partition(|pending| matches!(pending, Pending::Notice { .. }));
            inbox.pending = steering.into();
            notices
        };
        for notice in notices {
            if let Pending::Notice { source, text } = notice {
                self.record_notice(turn, source, text).await?;
            }
        }
        if let Some(focus) = prompt.compact {
            self.manual = true;
            self.manual_focus = focus;
            return Ok(());
        }
        let item = Item::UserMessage(UserMessageItem {
            id: self.item_ids.next_id(),
            parts: prompt.input,
            delivery: prompt.delivery,
            created_at: Some(self.clock.now().to_timestamp()),
        });
        self.record_shown(turn, item).await?;
        self.shared.lock().history = true;
        Ok(())
    }

    // ----------------------------------------------------------- preparing

    async fn prepare(&mut self, turn: &mut Turn, prep: Prep) -> Phase {
        if self.state != TurnState::Preparing {
            self.enter(TurnState::Preparing);
        }
        if turn.cancel.is_cancelled() {
            return Phase::Interrupted(None);
        }

        // This is a safe boundary: no tool round is open. Steering
        // messages and notices are delivered here, in the order they came.
        let pending: Vec<Pending> = self.shared.lock().pending.drain(..).collect();
        for pending in pending {
            let recorded = match pending {
                Pending::Steer(input) => {
                    let item = Item::UserMessage(UserMessageItem {
                        id: self.item_ids.next_id(),
                        parts: input,
                        delivery: Some(Delivery::Steer),
                        created_at: Some(self.clock.now().to_timestamp()),
                    });
                    self.record_shown(turn, item).await
                }
                Pending::Notice { source, text } => self.record_notice(turn, source, text).await,
                Pending::Compact(focus) => {
                    self.manual = true;
                    self.manual_focus = focus;
                    Ok(())
                }
            };
            if let Err(error) = recorded {
                return Phase::Finishing(End::store(&error));
            }
        }

        // The model of this request: what the role is set to now. A change
        // of it starts a context epoch, before anything else is sent.
        self.refresh_main();
        if let Err(error) = self.context_before_request(turn).await {
            return Phase::Finishing(End::store(&error));
        }
        if let Err(error) = self.collect_background(turn).await {
            return Phase::Finishing(End::store(&error));
        }
        if self.manual {
            return Phase::Compacting(prep);
        }
        match self.pressure() {
            Pressure::Compact if !prep.compacted => {
                if !self.context.parts.settings.auto_compact {
                    return Phase::Finishing(self.compaction_off());
                }
                return Phase::Compacting(prep);
            }
            Pressure::Compact | Pressure::Prepare => self.start_background(turn),
            Pressure::Fine => {}
        }

        // Every request is charged to the ledger before it is sent.
        if let Err(reached) = self.ledger.take_request() {
            let message = reached.message();
            self.notify(
                turn,
                Notice::Limit(LimitNotice {
                    message: message.clone(),
                    limit: reached.kind.clone(),
                    reached: true,
                }),
            );
            if let Err(error) = self
                .record_notice(turn, NoticeSource::Limit, message.clone())
                .await
            {
                return Phase::Finishing(End::store(&error));
            }
            return Phase::Finishing(End {
                outcome: TurnOutcome::Limit,
                error: Some(ErrorData::new(ErrorKind::Limit, reached.code(), message)),
            });
        }

        self.tell_speed();
        let model = self.model.clone();
        let assembled = self.context.assemble();
        let mut request = ModelRequest::new(Arc::clone(&model.entry), ModelRole::Main);
        request.system = assembled.system;
        request.tools = assembled.tools;
        request.messages = assembled.messages;
        request.cache = assembled.cache;
        request.reasoning = model.reasoning.clone();
        // The speed the role is set to. The client sends `standard` in its
        // place while the tier cools down or when the model has no such
        // tier, and says so in the stream.
        request.speed = self.shared.lock().speeds.of(&ModelRole::Main);
        request.max_output = if prep.raised_output {
            None
        } else {
            model.max_output
        };
        self.context_request(turn, &model, &request);
        turn.requests += 1;
        Phase::Requesting(Box::new(request), prep)
    }

    // ------------------------------------- requesting, streaming, backoff

    async fn respond(&mut self, turn: &mut Turn, request: ModelRequest, prep: Prep) -> Phase {
        self.enter(TurnState::Requesting);
        let model = self.model.clone();
        let mut live = Live {
            item_id: self.item_ids.next_id(),
            draft: Draft::default(),
            shown: false,
            attempt: 1,
            usage: None,
            sent: self.clock.monotonic(),
            first_content: None,
        };
        let mut round = Round::new(
            Arc::clone(&self.env),
            turn.id.clone(),
            turn.number,
            self.rounds_recorded + 1,
            turn.cancel.clone(),
            turn.kill.clone(),
        );
        // The request has a token of its own below the turn's. The loop
        // itself watches the turn's, and drops the stream when it fires:
        // that closes the connection, the one signal every provider honours.
        let mut stream = model.client.stream(request, turn.cancel.child());

        loop {
            let event = match wake(&turn.cancel, round.recv(), stream.next()).await {
                Woke::Cancelled => return Phase::Interrupted(Some(Box::new((live, round)))),
                Woke::Report(report) => {
                    round.handle(report);
                    round.pump(Stage::Streaming {
                        open_call: live.draft.first_open_call(),
                    });
                    continue;
                }
                // A client that ends its stream without a last word has
                // broken its promise. The request is over all the same.
                Woke::Event(event) => event.unwrap_or_else(|| {
                    ModelEvent::Error(ProviderError::new(
                        cortex_base::ErrorKind::Transient,
                        "incomplete_stream",
                        "The response ended before the provider said it was complete.",
                    ))
                }),
            };
            if self.state == TurnState::Backoff {
                // Whatever comes next, the wait is over and the request
                // was sent again.
                self.enter(TurnState::Requesting);
            }
            match event {
                ModelEvent::Start { .. } => {
                    if self.state == TurnState::Streaming {
                        // A response began over one that never ended: the
                        // earlier one no longer counts.
                        let error = ErrorData::new(
                            ErrorKind::Transient,
                            "incomplete_stream",
                            "The response was started again before it ended.",
                        );
                        if let Err(error) =
                            self.void(turn, &model, &mut live, &mut round, error).await
                        {
                            return self.abandon(round, End::store(&error)).await;
                        }
                    } else {
                        self.enter(TurnState::Streaming);
                    }
                    self.show(turn, &mut live);
                }
                ModelEvent::Retry(info) => {
                    let error = info.error.to_error_data_at(self.clock.now());
                    self.notify(
                        turn,
                        Notice::Retry(RetryNotice {
                            message: format!(
                                "The request failed ({}). Trying again in {} s (attempt {} of {}).",
                                info.error.code,
                                info.wait.as_secs_f32().ceil(),
                                info.attempt,
                                info.max_attempts
                            ),
                            attempt: info.attempt,
                            max_attempts: info.max_attempts,
                            error: error.clone(),
                            next_attempt_at: info.next_attempt_at.to_timestamp(),
                        }),
                    );
                    if let Err(error) = self.void(turn, &model, &mut live, &mut round, error).await
                    {
                        return self.abandon(round, End::store(&error)).await;
                    }
                    self.enter(TurnState::Backoff);
                }
                ModelEvent::SpeedFallback(moved) => {
                    self.speed_moved(turn, &model, &moved);
                    if moved.by == SpeedFallbackBy::Harness && self.state == TurnState::Streaming {
                        // The tier refused part-way and the request is sent
                        // again at standard speed: what arrived is void.
                        let error = ErrorData::new(
                            ErrorKind::Transient,
                            moved.reason.as_str(),
                            "The speed tier refused the request part-way. It was sent again at \
                             standard speed.",
                        );
                        if let Err(error) =
                            self.void(turn, &model, &mut live, &mut round, error).await
                        {
                            return self.abandon(round, End::store(&error)).await;
                        }
                        self.enter(TurnState::Requesting);
                    }
                }
                ModelEvent::SpeedCooldown(change) => self.cooldown_changed(turn, &model, &change),
                ModelEvent::Limits(windows) => {
                    self.usage
                        .set_limits(&model.entry.info.provider, windows, self.clock.now());
                    self.tell_usage(turn, &model, live.usage.as_ref(), false);
                }
                ModelEvent::Usage(usage) => {
                    live.usage = Some(usage);
                    self.tell_usage(turn, &model, live.usage.as_ref(), false);
                }
                ModelEvent::End {
                    stop,
                    raw_stop,
                    stop_detail,
                    usage,
                } => {
                    self.streaming(turn, &mut live);
                    return Phase::Deciding(Box::new(Response {
                        live,
                        round,
                        stop,
                        raw_stop,
                        stop_detail,
                        usage,
                        prep,
                    }));
                }
                ModelEvent::Error(error) => {
                    return self.failed(turn, &model, live, round, error, prep).await;
                }
                content => {
                    self.streaming(turn, &mut live);
                    match live.draft.apply(content) {
                        Applied::Nothing => {}
                        Applied::Delta(delta) => {
                            live.first_content
                                .get_or_insert_with(|| self.clock.monotonic());
                            self.emit(
                                turn,
                                Event::ItemDelta(ItemDelta {
                                    item_id: live.item_id.clone(),
                                    delta,
                                }),
                            );
                        }
                        // The call is planned at once, and runs now if it
                        // has no side effects.
                        Applied::Call(call) => {
                            live.first_content
                                .get_or_insert_with(|| self.clock.monotonic());
                            round.add_call(call, &mut self.item_ids);
                            round.pump(Stage::Streaming {
                                open_call: live.draft.first_open_call(),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Content or an end arrived: the response is streaming, whether or not
    /// the client said that it started.
    fn streaming(&mut self, turn: &Turn, live: &mut Live) {
        if self.state != TurnState::Streaming {
            self.enter(TurnState::Streaming);
        }
        self.show(turn, live);
    }

    /// Tells front ends the assistant item began, once.
    fn show(&self, turn: &Turn, live: &mut Live) {
        if !live.shown {
            live.shown = true;
            self.emit(
                turn,
                Event::ItemStarted(ItemStarted {
                    item_id: live.item_id.clone(),
                    kind: ItemKind::AssistantMessage,
                    tool: None,
                    call_id: None,
                }),
            );
        }
    }

    /// The attempt that was arriving no longer counts. Front ends clear
    /// what they showed of it, the log keeps it as an `attempt` entry, and
    /// the round keeps the runs that started for the next response.
    async fn void(
        &mut self,
        turn: &mut Turn,
        model: &RoleModel,
        live: &mut Live,
        round: &mut Round,
        error: ErrorData,
    ) -> Result<(), StoreError> {
        // What the provider said the attempt used was used.
        if let Some(usage) = live.usage.take() {
            self.account(turn, model, usage, live);
        }
        if live.shown {
            self.emit(
                turn,
                Event::ItemReset(ItemRef {
                    item_id: live.item_id.clone(),
                }),
            );
        }
        let attempt = Attempt {
            item_id: live.item_id.clone(),
            attempt: live.attempt,
            error,
            partial: live.draft.parts(Calls::Keep),
        };
        live.draft = Draft::default();
        live.attempt += 1;
        live.first_content = None;
        round.void();
        self.store.append(&turn.id, Record::Attempt(attempt)).await
    }

    /// Ends a turn whose response cannot go on, with nothing of the round
    /// left running.
    async fn abandon(&mut self, mut round: Round, end: End) -> Phase {
        round.void();
        round.discard_kept();
        round.settle().await;
        Phase::Finishing(end)
    }

    /// The request failed and will not be tried again by the client.
    async fn failed(
        &mut self,
        turn: &mut Turn,
        model: &RoleModel,
        mut live: Live,
        mut round: Round,
        error: ProviderError,
        prep: Prep,
    ) -> Phase {
        if error.kind == cortex_base::ErrorKind::Interrupted {
            return Phase::Interrupted(Some(Box::new((live, round))));
        }
        let data = error.to_error_data_at(self.clock.now());
        if error.kind == cortex_base::ErrorKind::ContextOverflow && !prep.overflowed {
            // The provider says the request does not fit: compact, once,
            // and send it again.
            if let Err(error) = self.void(turn, model, &mut live, &mut round, data).await {
                return self.abandon(round, End::store(&error)).await;
            }
            round.discard_kept();
            round.settle().await;
            return Phase::Compacting(Prep {
                overflowed: true,
                ..prep
            });
        }

        let usage = live
            .usage
            .take()
            .map(|usage| self.account(turn, model, usage, &live));
        turn.stop = Some(StopReason::Error);
        self.request_stat(
            turn,
            model,
            usage.clone(),
            StopReason::Error,
            Some(data.clone()),
            &live,
        );
        // None of the response's calls will run, and what ran early was
        // for a response that is not kept.
        let parts = live.draft.parts(Calls::Strip);
        let phase = self.abandon(round, End::failed(data.clone())).await;
        if live.shown {
            // A failure is data: the item ends with what arrived and the
            // error attached.
            let item = self.assistant_item(model, &live, parts, StopReason::Error, usage);
            let item = Item::AssistantMessage(AssistantMessageItem {
                error: Some(data),
                ..item
            });
            if let Err(error) = self.record_shown(turn, item).await {
                return Phase::Finishing(End::store(&error));
            }
        }
        phase
    }

    // ------------------------------------------------------------ deciding

    #[expect(
        clippy::too_many_lines,
        reason = "one stop reason after another, each a few lines; splitting it would hide the order they are looked at in"
    )]
    async fn decide(&mut self, turn: &mut Turn, response: Response) -> Phase {
        self.enter(TurnState::Deciding);
        let Response {
            mut live,
            mut round,
            stop,
            raw_stop,
            stop_detail,
            usage,
            prep,
        } = response;
        let model = self.model.clone();
        live.usage = None;
        let usage = self.account(turn, &model, usage, &live);
        let usage_counted = usage.clone();
        self.tell_usage(turn, &model, None, true);
        turn.stop = Some(stop.clone());

        if stop == StopReason::ContextOverflow && !prep.overflowed {
            self.request_stat(turn, &model, Some(usage), stop, None, &live);
            let error = ErrorData::new(
                ErrorKind::ContextOverflow,
                "context_overflow",
                "The request did not fit the model's context window.",
            );
            if let Err(error) = self.void(turn, &model, &mut live, &mut round, error).await {
                return self.abandon(round, End::store(&error)).await;
            }
            round.discard_kept();
            round.settle().await;
            return Phase::Compacting(Prep {
                overflowed: true,
                ..prep
            });
        }

        // Cut off inside a tool call: send the request once more with all
        // the output the model allows.
        let room_to_raise = model
            .max_output
            .is_some_and(|limit| limit < model.entry.info.max_output);
        if stop == StopReason::MaxOutput
            && live.draft.ends_in_unfinished_call()
            && !prep.raised_output
            && room_to_raise
        {
            self.request_stat(turn, &model, Some(usage), stop, None, &live);
            let error = ErrorData::new(
                ErrorKind::ModelStop,
                "max_output",
                "The response reached the output limit inside a tool call. The request was sent \
                 again with a higher limit.",
            );
            if let Err(error) = self.void(turn, &model, &mut live, &mut round, error).await {
                return self.abandon(round, End::store(&error)).await;
            }
            round.discard_kept();
            round.settle().await;
            return Phase::Preparing(Prep {
                raised_output: true,
                ..prep
            });
        }

        // The stops that end the turn as a failure. Their tool calls, if
        // the response has any, do not run.
        let detail = stop_detail
            .as_deref()
            .map_or_else(String::new, |detail| format!(" ({detail})"));
        let failure = match &stop {
            StopReason::Refusal => Some(ErrorData::new(
                ErrorKind::ModelStop,
                "refusal",
                format!("The model declined to answer{detail}."),
            )),
            StopReason::ContentFilter => Some(ErrorData::new(
                ErrorKind::ModelStop,
                "content_filter",
                format!("A content filter stopped the response{detail}."),
            )),
            StopReason::ContextOverflow => Some(ErrorData::new(
                ErrorKind::ContextOverflow,
                "context_overflow",
                "The request does not fit the model's context window, even after compaction.",
            )),
            StopReason::Error | StopReason::Interrupted => Some(ErrorData::new(
                ErrorKind::Provider,
                "response_failed",
                format!("The provider ended the response with an error{detail}."),
            )),
            _ => None,
        };
        let parts = if failure.is_some() {
            round.void();
            live.draft.parts(Calls::Strip)
        } else {
            live.draft.parts(Calls::Keep)
        };

        let item = self.assistant_item(&model, &live, parts, stop.clone(), Some(usage.clone()));
        let item = Item::AssistantMessage(AssistantMessageItem {
            raw_stop: Some(raw_stop.clone()),
            stop_detail,
            truncated: stop == StopReason::MaxOutput,
            error: failure.clone(),
            ..item
        });
        self.request_stat(
            turn,
            &model,
            Some(usage),
            stop.clone(),
            failure.clone(),
            &live,
        );
        if let Err(error) = self.record_shown(turn, item).await {
            return self.abandon(round, End::store(&error)).await;
        }
        // The provider counted the request, and its answer is in the
        // history: the next estimate goes on from here.
        self.context_counted(&usage_counted);
        if let Some(error) = failure {
            return self.abandon(round, End::failed(error)).await;
        }
        if round.has_calls() {
            return Phase::ToolRound(Box::new(round));
        }

        // No call to run. What ran for an earlier attempt and was not asked
        // for again goes.
        round.discard_kept();
        round.settle().await;
        match stop {
            // The provider paused a loop of its own: the response is kept
            // as it is and the next request continues it.
            StopReason::Paused => Phase::Preparing(Prep::default()),
            StopReason::MalformedToolCall => {
                let note = "Your last tool call could not be read, so nothing was run. Write the \
                            call again, with its arguments as one JSON object that fits the \
                            tool's schema.";
                if let Err(error) = self
                    .record_notice(turn, NoticeSource::Harness, note.to_owned())
                    .await
                {
                    return Phase::Finishing(End::store(&error));
                }
                self.guarded(turn, format!("malformed\u{0}{raw_stop}"))
                    .await
            }
            StopReason::MaxOutput => Phase::Finishing(End::failed(ErrorData::new(
                ErrorKind::ModelStop,
                "max_output",
                "The response reached the output limit and was cut off. Send a prompt to \
                 continue.",
            ))),
            _ => {
                if !turn.written.is_empty() && turn.verify_rounds < self.config.verify_rounds {
                    Phase::Verifying
                } else {
                    Phase::Finishing(End::completed())
                }
            }
        }
    }

    // ----------------------------------------------------------- tool round

    async fn tool_round(&mut self, turn: &mut Turn, mut round: Round) -> Phase {
        self.enter(TurnState::ToolRound);
        // The response has ended, so calls with side effects may start, and
        // whatever was kept and is not asked for again is discarded.
        let interrupted = loop {
            round.pump(Stage::Ended);
            if round.is_settled() {
                break false;
            }
            match turn.cancel.race(round.recv()).await {
                Some(report) => round.handle(report),
                None => {
                    // Every started call still gets its result.
                    round.interrupt();
                    round.discard_kept();
                    round.settle().await;
                    break true;
                }
            }
        };

        // Recorded in the model's call order, whatever order they finished
        // in, and never cut short: every provider rejects a call without a
        // result.
        let stopped = round.stopped().map(str::to_owned);
        let mut signature = String::new();
        if let Err(error) = self.record_results(turn, &mut round, &mut signature).await {
            return Phase::Finishing(End::store(&error));
        }
        if interrupted {
            return Phase::Interrupted(None);
        }
        if let Some(reason) = stopped {
            // The policy refused a call and said to stop: the results are
            // recorded, so the history is whole, and the turn ends here.
            let message =
                format!("A tool call was not allowed, and the turn was stopped: {reason}");
            self.notify(
                turn,
                Notice::Info(InfoNotice {
                    message: message.clone(),
                }),
            );
            return Phase::Finishing(End {
                outcome: TurnOutcome::Denied,
                error: Some(ErrorData::new(
                    ErrorKind::Permission,
                    "permission_denied",
                    message,
                )),
            });
        }
        self.guarded(turn, signature).await
    }

    /// Appends a settled round's results to the log and the context.
    async fn record_results(
        &mut self,
        turn: &mut Turn,
        round: &mut Round,
        signature: &mut String,
    ) -> Result<(), StoreError> {
        let model = self.model.clone();
        let mut recorded = false;
        for finished in round.finish() {
            recorded = true;
            turn.tool_calls += 1;
            self.stats.record(StatEvent::ToolCall(ToolCallStat {
                session: self.shared.info.id.clone(),
                turn: turn.id.clone(),
                tool: finished.item.tool.clone(),
                model: model.entry.info.id.clone(),
                variants: model.entry.variants.clone(),
                outcome: finished.stat,
                duration: finished.duration,
            }));
            signature.push_str(&finished.signature);
            turn.written.extend(finished.written);
            let item = Item::ToolResult(finished.item);
            self.store
                .append(&turn.id, Record::Item(item.clone()))
                .await?;
            self.context.push(&item);
        }
        if recorded {
            // Every result of the round is in the log and in the context:
            // the next request carries them. A round whose results could
            // not all be recorded returned above, and keeps its number.
            self.rounds_recorded += 1;
        }
        turn.written.sort();
        turn.written.dedup();
        Ok(())
    }

    /// Puts a round to the repetition guard, and goes on to the next
    /// request unless the guard stops the turn.
    async fn guarded(&mut self, turn: &Turn, signature: String) -> Phase {
        match self.guard.observe(signature) {
            Repetition::Fine => {}
            Repetition::Warn(count) => {
                let note = format!(
                    "The last {count} rounds made the same tool calls and got the same results. \
                     Repeating them changes nothing. Try another approach, or say what is in the \
                     way."
                );
                if let Err(error) = self
                    .record_notice(turn, NoticeSource::Repetition, note)
                    .await
                {
                    return Phase::Finishing(End::store(&error));
                }
            }
            Repetition::Stop(count) => {
                let message = format!(
                    "The model made the same tool calls {count} times in a row with the same \
                     results, so the turn was stopped. Say how to go on."
                );
                self.notify(
                    turn,
                    Notice::Info(InfoNotice {
                        message: message.clone(),
                    }),
                );
                return Phase::Finishing(End {
                    outcome: TurnOutcome::Limit,
                    error: Some(ErrorData::new(ErrorKind::Limit, "repetition", message)),
                });
            }
        }
        Phase::Preparing(Prep::default())
    }

    // ----------------------------------------------------------- verifying

    async fn verify(&mut self, turn: &mut Turn) -> Phase {
        self.enter(TurnState::Verifying);
        turn.verify_rounds += 1;
        let cx = VerifyCx {
            session: &self.shared.info.id,
            turn: &turn.id,
            round: turn.verify_rounds,
            written: &turn.written,
            cancel: turn.cancel.child(),
        };
        let Some(outcome) = turn.cancel.race(self.verifier.verify(cx)).await else {
            return Phase::Interrupted(None);
        };
        match outcome {
            VerifyOutcome::Passed => {
                turn.verification = Some(Verification::Passed);
                Phase::Finishing(End::completed())
            }
            VerifyOutcome::Skipped => {
                turn.verification = Some(Verification::Skipped);
                Phase::Finishing(End::completed())
            }
            VerifyOutcome::Failed { report } => {
                turn.verification = Some(Verification::Failed);
                if turn.verify_rounds >= self.config.verify_rounds {
                    // The round limit: the turn completes and reports the
                    // failing state.
                    return Phase::Finishing(End::completed());
                }
                match self
                    .record_notice(turn, NoticeSource::Verification, report)
                    .await
                {
                    Ok(()) => Phase::Preparing(Prep::default()),
                    Err(error) => Phase::Finishing(End::store(&error)),
                }
            }
        }
    }

    // ------------------------------------------------ finishing, interrupted

    async fn finish(&mut self, turn: &Turn, end: End) -> Option<(Running, Prompt)> {
        self.enter(TurnState::Finishing);
        let completed = TurnCompleted {
            verification: (end.outcome == TurnOutcome::Completed)
                .then(|| turn.verification.clone().unwrap_or(Verification::Skipped)),
            outcome: end.outcome,
            stop_reason: turn.stop.clone(),
            error: end.error,
            usage: turn.usage.clone(),
            duration_ms: Some(millis(self.clock.monotonic().saturating_sub(turn.started))),
        };
        self.conclude(turn, completed).await
    }

    async fn interrupted(&mut self, turn: &mut Turn, partial: Option<Box<(Live, Round)>>) {
        self.enter(TurnState::Interrupted);
        // However the turn got here, everything below it stops, a summary
        // in the background included.
        turn.cancel.cancel();
        self.stop_background();
        if let Some(partial) = partial {
            let (mut live, mut round) = *partial;
            let model = self.model.clone();
            let usage = live
                .usage
                .take()
                .map(|usage| self.account(turn, &model, usage, &live));
            // The calls whose arguments were complete each get a result:
            // what a run that had started returns, or "interrupted".
            round.interrupt();
            round.discard_kept();
            round.settle().await;
            if live.shown {
                // What arrived is kept and marked interrupted. A call whose
                // arguments were still arriving is left out, and never runs.
                let parts = live.draft.parts(Calls::Keep);
                let item = Item::AssistantMessage(self.assistant_item(
                    &model,
                    &live,
                    parts,
                    StopReason::Interrupted,
                    usage,
                ));
                let mut signature = String::new();
                // If the log cannot be written there is nothing more to do
                // about it here: the turn ends as interrupted either way,
                // and the session is read again before the next one.
                let recorded = match self.record_shown(turn, item).await {
                    Ok(()) => self.record_results(turn, &mut round, &mut signature).await,
                    Err(error) => Err(error),
                };
                if recorded.is_err() {
                    self.shared.lock().reload = true;
                }
            }
        }
        turn.stop = Some(StopReason::Interrupted);
        let completed = TurnCompleted {
            outcome: TurnOutcome::Interrupted,
            stop_reason: Some(StopReason::Interrupted),
            error: None,
            usage: turn.usage.clone(),
            verification: None,
            duration_ms: Some(millis(self.clock.monotonic().saturating_sub(turn.started))),
        };
        self.conclude(turn, completed).await;
    }

    /// The end of every turn: the log is told how it ended, front ends get
    /// `turn.completed`, and the session either starts the next queued
    /// prompt or is idle.
    ///
    /// **Only a turn that completed starts the next prompt by itself.**
    /// After any other end the prompts that were waiting are held: they
    /// follow the next prompt the user sends.
    async fn conclude(
        &mut self,
        turn: &Turn,
        completed: TurnCompleted,
    ) -> Option<(Running, Prompt)> {
        let stored = self.store.end_turn(&turn.id, &completed).await;
        let store_failed = stored.is_err()
            || completed
                .error
                .as_ref()
                .is_some_and(|error| error.code == "store_failed");
        let completed = match stored {
            Err(error) if completed.error.is_none() && self.state == TurnState::Finishing => {
                TurnCompleted {
                    outcome: TurnOutcome::Failed,
                    error: Some(error.to_error_data()),
                    verification: None,
                    ..completed
                }
            }
            _ => completed,
        };
        let outcome = completed.outcome.clone();
        self.stats.record(StatEvent::Turn(TurnStat {
            session: self.shared.info.id.clone(),
            turn: turn.id.clone(),
            model: self.model.entry.info.id.clone(),
            requests: turn.requests,
            tool_calls: turn.tool_calls,
            outcome: outcome.clone(),
            verification: completed.verification.clone(),
            duration: self.clock.monotonic().saturating_sub(turn.started),
        }));
        self.emit(turn, Event::TurnCompleted(completed));

        let next = {
            let mut inbox = self.shared.lock();
            // An interrupt that came as the turn was finishing does not
            // change how the turn ended. It still means stop: nothing
            // follows by itself. The same holds for a session that is
            // being closed.
            let next = if outcome == TurnOutcome::Completed && !turn.cancel.is_cancelled() {
                self.next_turn(&mut inbox)
            } else {
                Self::hold(&mut inbox);
                None
            };
            inbox.running = next.as_ref().map(|(running, _)| running.clone());
            if store_failed {
                // Memory is brought back to what the log holds.
                inbox.reload = true;
            }
            // Told under the lock, so that a prompt accepted from another
            // thread is not followed by a stale "idle".
            self.shared.emit_updated(&inbox);
            if next.is_none() && inbox.queue.is_empty() && inbox.held.is_empty() {
                self.shared
                    .emitter
                    .emit(None, Event::SessionIdle(SessionIdle {}));
            }
            next
        };
        if next.is_none() {
            self.enter(TurnState::Idle);
        }
        next
    }

    /// The prompt that follows a completed turn: a steering message that
    /// found no boundary to be delivered at, and otherwise the first queued
    /// prompt.
    fn next_turn(&self, inbox: &mut Inbox) -> Option<(Running, Prompt)> {
        let steering = inbox
            .pending
            .iter()
            .position(|pending| matches!(pending, Pending::Steer(_)));
        let (turn, prompt) = if let Some(position) = steering
            && let Some(Pending::Steer(input)) = inbox.pending.remove(position)
        {
            let prompt = Prompt {
                input,
                delivery: Some(Delivery::Steer),
                compact: None,
            };
            (inbox.turn_ids.next_id(), prompt)
        } else {
            let queued = inbox.queue.pop_front()?;
            (queued.turn, queued.prompt)
        };
        let running = Running {
            id: turn,
            cancel: self.shared.cancel.child(),
            kill: CancelToken::new(),
        };
        Some((running, prompt))
    }

    /// Holds everything that was waiting behind a turn that did not
    /// complete: steering messages that were never delivered, then the
    /// queue, in order.
    fn hold(inbox: &mut Inbox) {
        let (steering, notices): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut inbox.pending)
            .into_iter()
            .partition(|pending| matches!(pending, Pending::Steer(_)));
        inbox.pending = notices.into();
        let mut held = std::mem::take(&mut inbox.held);
        for pending in steering {
            if let Pending::Steer(input) = pending {
                held.push_back(Queued {
                    turn: inbox.turn_ids.next_id(),
                    prompt: Prompt {
                        input,
                        delivery: Some(Delivery::Steer),
                        compact: None,
                    },
                });
            }
        }
        held.extend(inbox.queue.drain(..));
        inbox.held = held;
    }

    // -------------------------------------------------------------- helpers

    fn enter(&mut self, next: TurnState) {
        let from = self.state;
        debug_assert!(
            from.may_become(next),
            "the loop went from {from} to {next}, which the table does not have"
        );
        self.state = next;
        self.shared.lock().state = next;
        if let Some(observer) = &self.observer {
            observer.transition(&self.shared.info.id, from, next);
        }
    }

    fn emit(&self, turn: &Turn, event: Event) {
        self.shared.emitter.emit(Some(&turn.id), event);
    }

    fn notify(&self, turn: &Turn, notice: Notice) {
        self.emit(turn, Event::Notice(notice));
    }

    /// Appends an item to the log, and only then to the context: memory
    /// never holds what the log does not.
    async fn record(&mut self, turn: &Turn, item: &Item) -> Result<(), StoreError> {
        self.store
            .append(&turn.id, Record::Item(item.clone()))
            .await?;
        self.context.push(item);
        Ok(())
    }

    /// Records an item and tells front ends it is complete.
    async fn record_shown(&mut self, turn: &Turn, item: Item) -> Result<(), StoreError> {
        self.record(turn, &item).await?;
        self.emit(turn, Event::ItemCompleted(ItemCompleted { item }));
        Ok(())
    }

    /// Adds a message from the harness to the conversation.
    async fn record_notice(
        &mut self,
        turn: &Turn,
        source: NoticeSource,
        text: String,
    ) -> Result<(), StoreError> {
        let item = Item::Notice(NoticeItem {
            id: self.item_ids.next_id(),
            source,
            text,
            created_at: Some(self.clock.now().to_timestamp()),
        });
        self.record_shown(turn, item).await
    }

    fn assistant_item(
        &self,
        model: &RoleModel,
        live: &Live,
        parts: Vec<Part>,
        stop: StopReason,
        usage: Option<RequestUsage>,
    ) -> AssistantMessageItem {
        let info = &model.entry.info;
        AssistantMessageItem {
            id: live.item_id.clone(),
            parts,
            stop_reason: Some(stop),
            raw_stop: None,
            stop_detail: None,
            truncated: false,
            origin: Some(Origin {
                protocol: info.protocol.clone(),
                provider: info.provider.clone(),
                model: info.id.clone(),
                account_scope: None,
            }),
            usage,
            error: None,
            created_at: Some(self.clock.now().to_timestamp()),
        }
    }

    /// The record of what a request used, priced at the speed it ran at.
    /// `timing` is when the request was sent and when its first content
    /// arrived, for a request that has ended.
    fn usage_record(
        &self,
        model: &RoleModel,
        usage: Usage,
        timing: Option<(Duration, Option<Duration>)>,
    ) -> RequestUsage {
        self.usage_record_as(model, ModelRole::Main, usage, timing)
    }

    /// [`usage_record`](Self::usage_record) for a request of any role.
    fn usage_record_as(
        &self,
        model: &RoleModel,
        role: ModelRole,
        usage: Usage,
        timing: Option<(Duration, Option<Duration>)>,
    ) -> RequestUsage {
        let mut record = cortex_llm::usage::request_usage(
            usage,
            &model.entry.info,
            role,
            model.billing.clone(),
            &self.catalog_version,
        );
        if let Some((sent, first_content)) = timing {
            let now = self.clock.monotonic();
            record.duration_ms = Some(millis(now.saturating_sub(sent)));
            record.time_to_first_token_ms =
                first_content.map(|first| millis(first.saturating_sub(sent)));
        }
        record
    }

    /// Counts what a request used: in the session's usage, in the turn's,
    /// and against every account of the ledger.
    fn account(
        &mut self,
        turn: &mut Turn,
        model: &RoleModel,
        usage: Usage,
        live: &Live,
    ) -> RequestUsage {
        let record = self.usage_record(model, usage, Some((live.sent, live.first_content)));
        self.usage.add(&record);
        self.ledger.charge(&record);
        add_request(&mut turn.usage, &record);
        if record.speed_requested.is_none() {
            // It ran at the speed that was asked for: the next fallback is
            // news again.
            self.moves.forget(&ModelRole::Main);
        }
        self.shared.lock().usage = self.usage.snapshot();
        record
    }

    /// Sends `usage.updated`: at most once a frame while a response
    /// arrives, and always when `now` is set.
    fn tell_usage(&mut self, turn: &Turn, model: &RoleModel, running: Option<&Usage>, now: bool) {
        let time = self.clock.monotonic();
        if !now
            && self
                .usage_sent
                .is_some_and(|sent| time.saturating_sub(sent) < self.config.usage_interval)
        {
            return;
        }
        self.usage_sent = Some(time);
        let usage = match running {
            Some(running) => {
                self.usage
                    .with_running(&self.usage_record(model, running.clone(), None))
            }
            None => self.usage.snapshot(),
        };
        self.emit(turn, Event::UsageUpdated(UsageUpdated { usage }));
    }

    fn request_stat(
        &mut self,
        turn: &Turn,
        model: &RoleModel,
        usage: Option<RequestUsage>,
        stop: StopReason,
        error: Option<ErrorData>,
        live: &Live,
    ) {
        let tokens = usage.as_ref().map(|usage| usage.tokens.clone());
        self.stats.record(StatEvent::Request(RequestStat {
            session: self.shared.info.id.clone(),
            turn: turn.id.clone(),
            at: self.clock.now(),
            provider: model.entry.info.provider.clone(),
            model: model.entry.info.id.clone(),
            variants: model.entry.variants.clone(),
            usage,
            stop,
            error,
            retries: live.attempt.saturating_sub(1),
        }));
        if let Some(mut context) = self.pending_context.take() {
            context.usage = tokens;
            self.stats.record(StatEvent::Context(context));
        }
    }

    /// A request runs, or ran, at another speed than its role is set to.
    fn speed_moved(&mut self, turn: &Turn, model: &RoleModel, moved: &SpeedMove) {
        if self.moves.is_news(&ModelRole::Main, moved) {
            self.notify(
                turn,
                Notice::SpeedFallback(fallback_notice(&ModelRole::Main, &model.entry.info, moved)),
            );
        }
        self.tell_speed();
    }

    /// A speed tier's cooldown began or ended.
    fn cooldown_changed(&mut self, turn: &Turn, model: &RoleModel, change: &CooldownChange) {
        self.notify(
            turn,
            Notice::SpeedCooldown(cooldown_notice(&model.entry.info, change)),
        );
        if change.state == CooldownState::Ended {
            self.moves.forget(&ModelRole::Main);
        }
        self.tell_speed();
    }

    /// Keeps the session state's speed in effect current: when what a
    /// role's next request will ask for has changed, front ends are told.
    fn tell_speed(&mut self) {
        let inbox = self.shared.lock();
        let roles: Vec<RoleState> = self.shared.state(&inbox).roles.into_values().collect();
        if roles != self.told_roles {
            self.told_roles = roles;
            self.shared.emit_updated(&inbox);
        }
    }
}
