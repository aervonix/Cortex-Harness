//! One tool round: the calls of one response, planned, approved, run and
//! handed back in the model's order
//! (`docs/architecture/03-agent-loop.md`, "Tool rounds").
//!
//! A [`Round`] begins with the request, not with the end of the response:
//! a call is planned the moment its block ends, a question is put the
//! moment the plan needs one, and a call with no side effects starts
//! running while the model is still writing.
//!
//! # How it runs
//!
//! Planning, a question and a tool run are each a task. A task reports on
//! one channel, and the round changes only when the loop hands it a report
//! ([`Round::handle`]), so every decision about what may start is made in
//! one place, [`Round::pump`], by these rules:
//!
//! - a call waits for every **earlier** call it conflicts with
//!   ([`Plan::conflicts_with`]), and for every earlier call whose plan is
//!   not known yet;
//! - while the response is still arriving only calls **without side
//!   effects** start;
//! - no more than the limit run at once;
//! - questions are asked one at a time, in call order.
//!
//! # Exactly one result
//!
//! Every call the round is given ends with one result, whatever happens:
//! the tool's own, an error for arguments that do not fit, a denial, or
//! "interrupted". A task that panics reports an error instead of nothing.
//!
//! # When a response is thrown away
//!
//! A response can fail part-way and be sent again. Its calls are then void
//! ([`Round::void`]). The ones that already ran had no side effects, and
//! their results are kept in a cache for as long as the request lasts: if
//! the new response asks for the same call, the result is used again. One
//! that is not asked for again is discarded, and so that nothing later
//! mistakes it for something the model saw, the files it read are forgotten
//! by the session's table of seen files.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use cortex_base::{AbsPath, CancelToken, IdCounter, SharedClock};
use cortex_protocol::event::{
    Delta, Event, ItemCompleted, ItemDelta, ItemKind, ItemRef, ItemStarted, TextDelta,
};
use cortex_protocol::item::{ToolOutcome, ToolResultItem};
use cortex_protocol::{CallId, Item, ItemId, Part, SessionId, TurnId};
use cortex_tools::{
    BlockingError, Effect, Json, Plan, RunCx, Tool, ToolError, ToolOutput, ToolResult, bound,
    execute, run_blocking,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::assistant::EndedCall;
use crate::config::Workspace;
use crate::events::Emitter;
use crate::policy::{Decision, PlannedCall, Policy, Verdict};
use crate::stats::ToolStatOutcome;
use crate::toolset::ToolSet;

/// What every round of a session shares.
pub(crate) struct RoundEnv {
    pub(crate) session: SessionId,
    pub(crate) tools: Arc<dyn ToolSet>,
    pub(crate) policy: Arc<dyn Policy>,
    pub(crate) workspace: Workspace,
    pub(crate) emitter: Arc<Emitter>,
    pub(crate) clock: SharedClock,
    /// The most calls that run at once.
    pub(crate) max_parallel: usize,
}

/// A report from one of the round's tasks.
pub(crate) enum Report {
    /// A call was planned, and the policy has looked at the plan.
    Planned {
        token: u64,
        plan: Plan,
        decision: Decision,
    },
    /// A question was answered.
    Answered {
        token: u64,
        plan: Plan,
        verdict: Verdict,
    },
    /// A call has its result.
    Finished {
        token: u64,
        result: ToolResult,
        duration: Duration,
        stat: ToolStatOutcome,
    },
}

/// What a planned call touches, kept after the plan itself has gone to the
/// tool.
struct Footprint {
    /// A plan with the same effects and scheduling, for
    /// [`Plan::conflicts_with`].
    shadow: Plan,
    side_effects: bool,
    /// Every path an effect names.
    paths: Vec<AbsPath>,
    /// The files it reads one by one: what the table of seen files may
    /// have recorded for it.
    reads: Vec<AbsPath>,
    /// The files it writes.
    writes: Vec<AbsPath>,
}

impl Footprint {
    fn of(plan: &Plan) -> Self {
        let mut shadow =
            Plan::new(plan.summary.clone(), ()).with_concurrency(plan.concurrency.clone());
        shadow.effects.clone_from(&plan.effects);
        let mut footprint = Self {
            shadow,
            side_effects: plan.has_side_effects(),
            paths: Vec::new(),
            reads: Vec::new(),
            writes: Vec::new(),
        };
        for effect in &plan.effects {
            if let Some(path) = effect.path() {
                footprint.paths.push(path.abs().clone());
            }
            match effect {
                Effect::Read(path) => footprint.reads.push(path.abs().clone()),
                Effect::Write(path) => footprint.writes.push(path.abs().clone()),
                _ => {}
            }
        }
        footprint
    }
}

/// Whether any path of one list is a path of the other, or lies inside
/// one.
fn overlap(first: &[AbsPath], second: &[AbsPath]) -> bool {
    first.iter().any(|one| {
        second
            .iter()
            .any(|other| one.starts_with(other) || other.starts_with(one))
    })
}

/// A call's result, ready to record.
struct Outcome {
    item: ToolResultItem,
    stat: ToolStatOutcome,
    duration: Duration,
}

enum CallState {
    /// A task is at work on it and will report: planning it, or bounding
    /// a result it gets without running.
    Pending,
    /// Planned, and the policy says to ask.
    NeedsAnswer(Plan),
    /// Its question is open.
    Asking,
    /// Allowed, and waiting for its turn to run.
    Ready(Plan),
    /// Running.
    Running,
    /// It has its result.
    Done(Outcome),
}

struct Call {
    /// What its tasks report under.
    token: u64,
    /// The index of its block in the response.
    index: u32,
    call_id: CallId,
    item_id: ItemId,
    name: String,
    arguments: Json,
    /// What a repeat of the call is recognised by.
    key: String,
    tool: Option<Arc<dyn Tool>>,
    footprint: Option<Footprint>,
    state: CallState,
    cancel: CancelToken,
    /// Whether front ends have been told the item began.
    shown: bool,
    /// Whether a tool run was started for it.
    ran: bool,
}

/// A run from a response that was thrown away, kept in case the next
/// response asks for the same call.
struct Cached {
    token: u64,
    name: String,
    key: String,
    item_id: ItemId,
    footprint: Footprint,
    cancel: CancelToken,
    /// `None` while it is still running.
    outcome: Option<Outcome>,
}

/// A discarded run that has been cancelled and has not reported yet. Until
/// it does, it may still write to the table of seen files, so nothing that
/// touches the same files starts.
struct Draining {
    token: u64,
    paths: Vec<AbsPath>,
    reads: Vec<AbsPath>,
}

/// One call's result, as the loop records it.
pub(crate) struct Finished {
    pub(crate) item: ToolResultItem,
    pub(crate) stat: ToolStatOutcome,
    pub(crate) duration: Duration,
    /// The files the call wrote, if it succeeded.
    pub(crate) written: Vec<AbsPath>,
    /// What a repeat of the call with the same result is recognised by.
    pub(crate) signature: String,
}

/// The tool calls of one request.
pub(crate) struct Round {
    env: Arc<RoundEnv>,
    turn: TurnId,
    turn_number: u32,
    /// The turn's token. Every call's token is a child of it.
    cancel: CancelToken,
    tx: UnboundedSender<Report>,
    rx: UnboundedReceiver<Report>,
    /// The calls of the response that counts, in the order their blocks
    /// ended.
    calls: Vec<Call>,
    cache: Vec<Cached>,
    draining: Vec<Draining>,
    /// The open question: the call's token, and the task that asks.
    asking: Option<(u64, AbortHandle)>,
    next_token: u64,
}

/// The text of the result of a call that never ran because the turn was
/// interrupted.
const NOT_RUN: &str = "The turn was interrupted before this call ran.";

impl Round {
    pub(crate) fn new(
        env: Arc<RoundEnv>,
        turn: TurnId,
        turn_number: u32,
        cancel: CancelToken,
    ) -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            env,
            turn,
            turn_number,
            cancel,
            tx,
            rx,
            calls: Vec::new(),
            cache: Vec::new(),
            draining: Vec::new(),
            asking: None,
            next_token: 0,
        }
    }

    /// Whether the response that counts has any call.
    pub(crate) fn has_calls(&self) -> bool {
        !self.calls.is_empty()
    }

    /// Whether every call has its result and no discarded run is still to
    /// report.
    pub(crate) fn is_settled(&self) -> bool {
        self.draining.is_empty()
            && self
                .calls
                .iter()
                .all(|call| matches!(call.state, CallState::Done(_)))
    }

    /// The next report from one of the round's tasks.
    pub(crate) async fn recv(&mut self) -> Report {
        match self.rx.recv().await {
            Some(report) => report,
            // The round holds a sender itself, so the channel never closes.
            None => std::future::pending().await,
        }
    }

    /// Waits until the round is settled, taking reports as they come and
    /// starting nothing new.
    pub(crate) async fn settle(&mut self) {
        while !self.is_settled() {
            let report = self.recv().await;
            self.handle(report);
        }
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    fn run_cx(&self, cancel: CancelToken) -> RunCx {
        let workspace = &self.env.workspace;
        let mut cx = RunCx::new(
            Arc::clone(&workspace.paths),
            Arc::clone(&workspace.content),
            Arc::clone(&workspace.files),
        )
        .with_cancel(cancel)
        .with_limits(workspace.limits)
        .with_gate(self.env.policy.read_gate())
        .with_turn(self.turn_number);
        if let Some(outliner) = &workspace.outliner {
            cx = cx.with_outliner(Arc::clone(outliner));
        }
        if let Some(images) = &workspace.images {
            cx = cx.with_images(images.clone());
        }
        cx
    }

    /// Runs `work` as a task that reports once, whatever happens: if the
    /// work panics, the report is an error result for the call.
    fn spawn(
        &self,
        token: u64,
        name: &str,
        work: impl Future<Output = Report> + Send + 'static,
    ) -> AbortHandle {
        let tx = self.tx.clone();
        let cx = self.run_cx(CancelToken::new());
        let name = name.to_owned();
        tokio::spawn(async move {
            let report = match Guarded(Box::pin(work)).await {
                Ok(report) => report,
                Err(panic) => Report::Finished {
                    token,
                    result: bound(
                        ToolResult::error(format!(
                            "internal error: the call panicked ({panic}). It did not finish."
                        )),
                        &name,
                        &cx,
                    )
                    .await,
                    duration: Duration::ZERO,
                    stat: ToolStatOutcome::Failed,
                },
            };
            // The round is gone only when the session is: nobody is left
            // to tell.
            let _ = tx.send(report);
        })
        .abort_handle()
    }

    /// Gives a call a result it gets without running, through the output
    /// policy.
    fn settle_with(&self, token: u64, name: &str, result: ToolResult, stat: ToolStatOutcome) {
        let cx = self.run_cx(CancelToken::new());
        let tool = name.to_owned();
        self.spawn(token, name, async move {
            Report::Finished {
                token,
                result: bound(result, &tool, &cx).await,
                duration: Duration::ZERO,
                stat,
            }
        });
    }

    /// A tool call's block ended: the call joins the round and is planned
    /// at once.
    pub(crate) fn add_call(&mut self, ended: EndedCall, ids: &mut IdCounter) {
        let EndedCall {
            index,
            id: call_id,
            name,
            arguments,
        } = ended;
        let arguments = match arguments {
            Ok(arguments) => arguments,
            Err(problem) => {
                let call = self.new_call(index, call_id, &name, Json::Null, String::new(), ids);
                self.settle_with(
                    call.token,
                    &name,
                    ToolResult::error(format!(
                        "The arguments of this call were not valid JSON ({problem}). Send the \
                         arguments as one JSON object that fits the tool's schema."
                    )),
                    ToolStatOutcome::InvalidArguments,
                );
                self.calls.push(call);
                return;
            }
        };

        let key = format!("{name}\u{0}{arguments}");
        if let Some(position) = self.cache.iter().position(|cached| cached.key == key) {
            self.adopt(position, index, call_id, name, arguments);
            return;
        }

        let mut call = self.new_call(index, call_id, &name, arguments, key, ids);
        match self.env.tools.get(&name) {
            Some(tool) => {
                self.plan(&call, &tool);
                call.tool = Some(tool);
            }
            None => {
                let known: Vec<&str> = self
                    .env
                    .tools
                    .specs()
                    .iter()
                    .map(|spec| spec.name.as_str())
                    .collect();
                self.settle_with(
                    call.token,
                    &name,
                    ToolResult::error(format!(
                        "There is no tool `{name}`. The tools are: {}.",
                        known.join(", ")
                    )),
                    ToolStatOutcome::UnknownTool,
                );
            }
        }
        self.calls.push(call);
    }

    fn new_call(
        &mut self,
        index: u32,
        call_id: CallId,
        name: &str,
        arguments: Json,
        key: String,
        ids: &mut IdCounter,
    ) -> Call {
        Call {
            token: self.token(),
            index,
            call_id,
            item_id: ids.next_id(),
            name: name.to_owned(),
            arguments,
            key,
            tool: None,
            footprint: None,
            state: CallState::Pending,
            cancel: self.cancel.child(),
            shown: false,
            ran: false,
        }
    }

    /// Starts the task that plans a call and shows the plan to the policy.
    fn plan(&self, call: &Call, tool: &Arc<dyn Tool>) {
        let token = call.token;
        let tool = Arc::clone(tool);
        let policy = Arc::clone(&self.env.policy);
        let session = self.env.session.clone();
        let turn = self.turn.clone();
        let call_id = call.call_id.clone();
        let arguments = call.arguments.clone();
        let name = call.name.clone();
        let cx = self.run_cx(call.cancel.clone());
        self.spawn(token, &call.name, async move {
            let planner = Arc::clone(&tool);
            let plan_cx = cx.plan_cx();
            let plan_arguments = arguments.clone();
            // Planning may ask the file system where a path leads, which
            // blocks.
            let planned =
                run_blocking(&cx.cancel, move |_| planner.plan(&plan_arguments, &plan_cx)).await;
            let (result, stat) = match planned {
                Ok(Ok(plan)) => {
                    let decision = policy.decide(&PlannedCall {
                        session: &session,
                        turn: &turn,
                        call_id: &call_id,
                        spec: tool.spec(),
                        arguments: &arguments,
                        plan: &plan,
                    });
                    return Report::Planned {
                        token,
                        plan,
                        decision,
                    };
                }
                Ok(Err(error)) => {
                    let stat = match &error {
                        ToolError::Internal(_) => ToolStatOutcome::Failed,
                        ToolError::UnknownTool(_) => ToolStatOutcome::UnknownTool,
                        _ => ToolStatOutcome::InvalidArguments,
                    };
                    (error.into_result(), stat)
                }
                Err(BlockingError::Cancelled) => (
                    ToolResult::interrupted(NOT_RUN),
                    ToolStatOutcome::Interrupted,
                ),
                Err(BlockingError::Panicked(message)) => (
                    ToolResult::error(format!(
                        "internal error: planning the call panicked ({message})."
                    )),
                    ToolStatOutcome::Failed,
                ),
            };
            Report::Finished {
                token,
                result: bound(result, &name, &cx).await,
                duration: Duration::ZERO,
                stat,
            }
        });
    }

    /// The new response asks for a call that already ran for the response
    /// before it: the run, or its result, is used again.
    fn adopt(
        &mut self,
        position: usize,
        index: u32,
        call_id: CallId,
        name: String,
        arguments: Json,
    ) {
        let cached = self.cache.remove(position);
        let state = match cached.outcome {
            Some(mut outcome) => {
                outcome.item.call_id = call_id.clone();
                CallState::Done(outcome)
            }
            None => CallState::Running,
        };
        let call = Call {
            token: cached.token,
            index,
            call_id,
            item_id: cached.item_id,
            name,
            arguments,
            key: cached.key,
            tool: None,
            footprint: Some(cached.footprint),
            state,
            cancel: cached.cancel,
            shown: true,
            ran: true,
        };
        self.emit_started(&call);
        if let CallState::Done(outcome) = &call.state {
            self.emit_completed(&outcome.item);
        }
        self.calls.push(call);
    }

    /// Takes one report and changes the round by it. Nothing is started
    /// here: [`pump`](Self::pump) does that.
    pub(crate) fn handle(&mut self, report: Report) {
        match report {
            Report::Planned {
                token,
                plan,
                decision,
            } => self.planned(token, plan, decision),
            Report::Answered {
                token,
                plan,
                verdict,
            } => self.answered(token, plan, verdict),
            Report::Finished {
                token,
                result,
                duration,
                stat,
            } => self.finished(token, result, duration, stat),
        }
    }

    fn position(&self, token: u64) -> Option<usize> {
        self.calls.iter().position(|call| call.token == token)
    }

    fn planned(&mut self, token: u64, plan: Plan, decision: Decision) {
        // A call of a response that was thrown away meanwhile: nothing to do.
        let Some(position) = self.position(token) else {
            return;
        };
        let footprint = Footprint::of(&plan);
        // A kept run that touched the same files is not what the model was
        // shown: it goes before this call can meet what it left behind.
        self.discard_where(|cached| overlap(&cached.footprint.paths, &footprint.paths));
        self.calls[position].footprint = Some(footprint);
        if self.cancel.is_cancelled() {
            self.not_run(position);
            return;
        }
        match decision {
            Decision::Allow => self.calls[position].state = CallState::Ready(plan),
            Decision::Ask => self.calls[position].state = CallState::NeedsAnswer(plan),
            Decision::Deny { reason } => {
                let mut text = "This call was not allowed to run.".to_owned();
                if let Some(reason) = reason {
                    text.push_str(" Reason: ");
                    text.push_str(&reason);
                }
                self.deny(position, text);
            }
        }
    }

    fn answered(&mut self, token: u64, plan: Plan, verdict: Verdict) {
        if self
            .asking
            .as_ref()
            .is_some_and(|(asked, _)| *asked == token)
        {
            self.asking = None;
        }
        let Some(position) = self.position(token) else {
            return;
        };
        if self.cancel.is_cancelled() {
            self.not_run(position);
            return;
        }
        match verdict {
            Verdict::Allow => self.calls[position].state = CallState::Ready(plan),
            Verdict::Deny { note } => {
                let mut text = "The user did not allow this call.".to_owned();
                if let Some(note) = note {
                    text.push_str(" They wrote: ");
                    text.push_str(&note);
                }
                self.deny(position, text);
            }
        }
    }

    fn deny(&mut self, position: usize, text: String) {
        let call = &mut self.calls[position];
        call.state = CallState::Pending;
        let (token, name) = (call.token, call.name.clone());
        self.settle_with(
            token,
            &name,
            ToolResult::denied(text),
            ToolStatOutcome::Denied,
        );
    }

    fn not_run(&mut self, position: usize) {
        let call = &mut self.calls[position];
        call.state = CallState::Pending;
        let (token, name) = (call.token, call.name.clone());
        self.settle_with(
            token,
            &name,
            ToolResult::interrupted(NOT_RUN),
            ToolStatOutcome::Interrupted,
        );
    }

    fn finished(
        &mut self,
        token: u64,
        result: ToolResult,
        duration: Duration,
        stat: ToolStatOutcome,
    ) {
        debug_assert!(result.is_bounded(), "every result passes the output policy");
        if let Some(position) = self.position(token) {
            let call = &self.calls[position];
            debug_assert!(
                !matches!(call.state, CallState::Done(_)),
                "a call has exactly one result"
            );
            let item = self.item(&call.item_id, &call.call_id, &call.name, result, duration);
            if !call.shown {
                self.emit_started(call);
            }
            self.emit_completed(&item);
            let call = &mut self.calls[position];
            call.shown = true;
            call.state = CallState::Done(Outcome {
                item,
                stat,
                duration,
            });
        } else if let Some(position) = self.cache.iter().position(|cached| cached.token == token) {
            // A kept run finished. Nobody is shown it unless the next
            // response asks for the same call, whose id then replaces the
            // empty one here.
            let cached = &self.cache[position];
            let item = self.item(
                &cached.item_id,
                &CallId::new(""),
                &cached.name,
                result,
                duration,
            );
            self.cache[position].outcome = Some(Outcome {
                item,
                stat,
                duration,
            });
        } else if let Some(position) = self.draining.iter().position(|run| run.token == token) {
            let run = self.draining.swap_remove(position);
            self.forget(&run.reads);
        }
        // Anything else is a report for a call that no longer exists.
    }

    fn item(
        &self,
        item_id: &ItemId,
        call_id: &CallId,
        name: &str,
        result: ToolResult,
        duration: Duration,
    ) -> ToolResultItem {
        let (outcome, content, payload, artifacts) = result.into_parts();
        ToolResultItem {
            id: item_id.clone(),
            call_id: call_id.clone(),
            tool: name.to_owned(),
            outcome,
            content,
            payload,
            artifacts,
            duration_ms: Some(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
            created_at: Some(self.env.clock.now().to_timestamp()),
        }
    }

    /// Starts whatever may start now. `streaming` is whether the response
    /// is still arriving, in which case only calls without side effects
    /// start.
    pub(crate) fn pump(&mut self, streaming: bool) {
        if self.cancel.is_cancelled() {
            return;
        }
        self.ask_next();

        let mut running = self
            .calls
            .iter()
            .filter(|call| matches!(call.state, CallState::Running))
            .count();
        for position in 0..self.calls.len() {
            if running >= self.env.max_parallel {
                break;
            }
            let call = &self.calls[position];
            let (CallState::Ready(_), Some(footprint)) = (&call.state, &call.footprint) else {
                continue;
            };
            if streaming && footprint.side_effects {
                continue;
            }
            let waits_for_earlier = self.calls[..position].iter().any(|earlier| {
                if matches!(earlier.state, CallState::Done(_)) {
                    return false;
                }
                // An earlier call that is not planned yet could turn out to
                // conflict.
                earlier
                    .footprint
                    .as_ref()
                    .is_none_or(|other| other.shadow.conflicts_with(&footprint.shadow))
            });
            let waits_for_discarded = self
                .draining
                .iter()
                .any(|run| overlap(&run.paths, &footprint.paths));
            if waits_for_earlier || waits_for_discarded {
                continue;
            }
            self.start(position);
            running += 1;
        }
    }

    /// Puts the next question, if none is open: the first call in order
    /// that needs an answer, once every call before it has been planned.
    fn ask_next(&mut self) {
        if self.asking.is_some() {
            return;
        }
        for position in 0..self.calls.len() {
            match &self.calls[position].state {
                CallState::Pending if self.calls[position].footprint.is_none() => return,
                CallState::NeedsAnswer(_) => {
                    self.ask(position);
                    return;
                }
                _ => {}
            }
        }
    }

    fn ask(&mut self, position: usize) {
        let call = &mut self.calls[position];
        let CallState::NeedsAnswer(plan) = std::mem::replace(&mut call.state, CallState::Asking)
        else {
            return;
        };
        let Some(tool) = call.tool.clone() else {
            // A call that was planned has its tool.
            call.state = CallState::Ready(plan);
            return;
        };
        let token = call.token;
        let policy = Arc::clone(&self.env.policy);
        let session = self.env.session.clone();
        let turn = self.turn.clone();
        let call_id = call.call_id.clone();
        let arguments = call.arguments.clone();
        let name = call.name.clone();
        let handle = self.spawn(token, &name, async move {
            let verdict = policy
                .ask(PlannedCall {
                    session: &session,
                    turn: &turn,
                    call_id: &call_id,
                    spec: tool.spec(),
                    arguments: &arguments,
                    plan: &plan,
                })
                .await;
            Report::Answered {
                token,
                plan,
                verdict,
            }
        });
        self.asking = Some((token, handle));
    }

    fn start(&mut self, position: usize) {
        let call = &mut self.calls[position];
        let CallState::Ready(plan) = std::mem::replace(&mut call.state, CallState::Running) else {
            return;
        };
        let Some(tool) = call.tool.clone() else {
            call.state = CallState::Ready(plan);
            return;
        };
        call.ran = true;
        call.shown = true;
        let token = call.token;
        let name = call.name.clone();
        let call = &self.calls[position];
        self.emit_started(call);

        let cx = self.run_cx(call.cancel.clone());
        let clock = Arc::clone(&self.env.clock);
        let mut out = OutputDeltas {
            emitter: Arc::clone(&self.env.emitter),
            turn: self.turn.clone(),
            item_id: call.item_id.clone(),
        };
        self.spawn(token, &name, async move {
            let started = clock.monotonic();
            let result = execute(tool.as_ref(), plan, &cx, &mut out).await;
            let stat = match result.outcome() {
                ToolOutcome::Success => ToolStatOutcome::Ok,
                ToolOutcome::Denied => ToolStatOutcome::Denied,
                ToolOutcome::Interrupted => ToolStatOutcome::Interrupted,
                _ => ToolStatOutcome::Failed,
            };
            Report::Finished {
                token,
                result,
                duration: clock.monotonic().saturating_sub(started),
                stat,
            }
        });
    }

    /// The response these calls came from no longer counts: it failed and
    /// is being sent again. Runs that started are kept for the next
    /// response; everything else is dropped.
    pub(crate) fn void(&mut self) {
        if let Some((_, question)) = self.asking.take() {
            // The question is withdrawn.
            question.abort();
        }
        for call in std::mem::take(&mut self.calls) {
            if call.shown {
                self.env.emitter.emit(
                    Some(&self.turn),
                    Event::ItemReset(ItemRef {
                        item_id: call.item_id.clone(),
                    }),
                );
            }
            let outcome = match call.state {
                CallState::Running => None,
                CallState::Done(outcome) if call.ran => Some(outcome),
                _ => {
                    // Not started: nothing to keep. A plan in progress stops.
                    call.cancel.cancel();
                    continue;
                }
            };
            // Only calls without side effects start before a response has
            // ended, so only those are ever kept.
            if let Some(footprint) = call.footprint {
                self.cache.push(Cached {
                    token: call.token,
                    name: call.name,
                    key: call.key,
                    item_id: call.item_id,
                    footprint,
                    cancel: call.cancel,
                    outcome,
                });
            }
        }
    }

    /// Discards every kept run: the request is over, and whatever the last
    /// response did not ask for again was never shown to the model.
    pub(crate) fn discard_kept(&mut self) {
        self.discard_where(|_| true);
    }

    fn discard_where(&mut self, discard: impl Fn(&Cached) -> bool) {
        let (gone, kept): (Vec<Cached>, Vec<Cached>) = std::mem::take(&mut self.cache)
            .into_iter()
            .partition(discard);
        self.cache = kept;
        for cached in gone {
            if cached.outcome.is_some() {
                self.forget(&cached.footprint.reads);
            } else {
                // Still running. It is told to stop, and may write to the
                // table of seen files until it has: the files are forgotten
                // when it reports.
                cached.cancel.cancel();
                self.draining.push(Draining {
                    token: cached.token,
                    paths: cached.footprint.paths,
                    reads: cached.footprint.reads,
                });
            }
        }
    }

    /// Makes the table of seen files forget what a discarded run recorded:
    /// the model was never shown it, so a later read must not be answered
    /// with "unchanged".
    fn forget(&self, reads: &[AbsPath]) {
        for path in reads {
            self.env.workspace.files.forget(path);
        }
    }

    /// The turn was interrupted. Calls that are running have been told to
    /// stop through their tokens and will report. Every call that has not
    /// started gets its result now: "interrupted", or for the one whose
    /// question was open, a denial.
    pub(crate) fn interrupt(&mut self) {
        if let Some((token, question)) = self.asking.take() {
            question.abort();
            if let Some(position) = self.position(token) {
                self.deny(
                    position,
                    "The turn was interrupted before anyone answered whether this call may run."
                        .to_owned(),
                );
            }
        }
        for position in 0..self.calls.len() {
            if matches!(
                self.calls[position].state,
                CallState::NeedsAnswer(_) | CallState::Ready(_)
            ) {
                self.not_run(position);
            }
        }
    }

    /// Puts the calls in the order of their blocks: the model's order,
    /// which is the order results are recorded in.
    pub(crate) fn seal(&mut self) {
        self.calls.sort_by_key(|call| call.index);
    }

    /// The results, in the model's call order. The round must be settled.
    pub(crate) fn finish(&mut self) -> Vec<Finished> {
        self.seal();
        std::mem::take(&mut self.calls)
            .into_iter()
            .filter_map(|call| {
                let CallState::Done(outcome) = call.state else {
                    debug_assert!(false, "every call of a settled round has its result");
                    return None;
                };
                let written = match (&outcome.item.outcome, call.footprint) {
                    (ToolOutcome::Success, Some(footprint)) => footprint.writes,
                    _ => Vec::new(),
                };
                let signature = format!(
                    "{}\u{0}{}\u{0}{}\u{0}{}\u{0}",
                    call.name,
                    call.key,
                    outcome.item.outcome.as_str(),
                    text_of(&outcome.item.content)
                );
                Some(Finished {
                    item: outcome.item,
                    stat: outcome.stat,
                    duration: outcome.duration,
                    written,
                    signature,
                })
            })
            .collect()
    }

    fn emit_started(&self, call: &Call) {
        self.env.emitter.emit(
            Some(&self.turn),
            Event::ItemStarted(ItemStarted {
                item_id: call.item_id.clone(),
                kind: ItemKind::ToolResult,
                tool: Some(call.name.clone()),
                call_id: Some(call.call_id.clone()),
            }),
        );
    }

    fn emit_completed(&self, item: &ToolResultItem) {
        self.env.emitter.emit(
            Some(&self.turn),
            Event::ItemCompleted(ItemCompleted {
                item: Item::ToolResult(item.clone()),
            }),
        );
    }
}

impl Drop for Round {
    fn drop(&mut self) {
        // A round is settled before it is dropped. If the loop itself is
        // torn down part-way, nothing it started is left running.
        if let Some((_, question)) = self.asking.take() {
            question.abort();
        }
        for call in &self.calls {
            call.cancel.cancel();
        }
        for cached in &self.cache {
            cached.cancel.cancel();
        }
    }
}

/// The text parts of a result, one after another.
fn text_of(content: &[Part]) -> String {
    let mut text = String::new();
    for part in content {
        if let Part::Text(part) = part {
            text.push_str(&part.text);
            text.push('\n');
        }
    }
    text
}

/// Sends a running tool's output to front ends as it arrives.
struct OutputDeltas {
    emitter: Arc<Emitter>,
    turn: TurnId,
    item_id: ItemId,
}

impl ToolOutput for OutputDeltas {
    fn output(&mut self, text: &str) {
        self.emitter.emit(
            Some(&self.turn),
            Event::ItemDelta(ItemDelta {
                item_id: self.item_id.clone(),
                delta: Delta::ToolOutput(TextDelta {
                    text: text.to_owned(),
                }),
            }),
        );
    }
}

/// Runs a future and turns a panic in it into an error, so that a task
/// always has something to report
/// (`docs/architecture/02-runtime.md`, "Panics").
struct Guarded<F>(Pin<Box<F>>);

impl<F: Future> Future for Guarded<F> {
    type Output = Result<F::Output, String>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.0.as_mut();
        // The future is dropped after a panic and never polled again, so
        // whatever state the panic left in it is not observed.
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(context))) {
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "no message".to_owned());
                Poll::Ready(Err(message))
            }
        }
    }
}
