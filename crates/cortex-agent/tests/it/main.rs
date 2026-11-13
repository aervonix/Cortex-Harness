//! Integration tests (tier 2): the loop, driven as the core will drive it,
//! with the scripted provider and fake tools.
//!
//! Nothing here waits for time. A session runs on a single-threaded
//! runtime, and a test waits for what it expects to see: an event, a line
//! in the tools' log, a gate. Where two things must happen in an order, one
//! side opens a [`Gate`] and the other waits for it.
//!
//! The pieces every test uses are in this file: [`Rig`], which puts a
//! session together and remembers everything it does; the fake tools
//! ([`Probe`]); and [`assert_valid_history`], which checks a request's
//! messages the way a provider would.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    missing_debug_implementations,
    unreachable_pub,
    reason = "an integration test prepares real directories and fails by panicking, and its \
              helpers are shared between the modules of this one binary"
)]

mod early;
mod interrupt;
mod limits;
mod retries;
mod rounds;
mod sessions;
mod speed;
mod steering;
mod streaming;
mod transitions;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_agent::{
    Assembled, Context, ContextError, Decision, EventSink, Ledger, LoopConfig, MemorySessionStore,
    Models, Observer, PlannedCall, Policy, RoleModel, Session, SessionHandle, SessionInfo,
    SessionParts, SessionStore, SpeedSettings, StatEvent, Stats, StubContext, TurnState, Verdict,
    Verifier, VerifyCx, VerifyOutcome, Workspace,
};
use cortex_base::{CancelToken, FakeClock, PathOptions, PathResolver, SharedClock};
use cortex_llm::{ModelRequest, SpeedCooldowns};
use cortex_protocol::event::{Delta, Event, ItemKind, Notice, SessionEvent, TurnCompleted};
use cortex_protocol::item::Delivery;
use cortex_protocol::methods::TurnStartResult;
use cortex_protocol::{Item, Message, Part, Role, SessionId, StopReason};
use cortex_testkit::{Gate, Response, ScriptedProvider, fixtures};
use cortex_tools::{
    BoxFuture, Concurrency, ConcurrencyClass, Effect, Json, Plan, PlanCx, RunCx, Tool, ToolError,
    ToolOutput, ToolResult, ToolSpec, Toolbox,
};
use serde_json::json;
use tokio::sync::Notify;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------ events

/// Everything a session told its front ends, in order.
#[derive(Default)]
pub struct Events {
    list: Mutex<Vec<SessionEvent>>,
    wake: Notify,
    /// Set by the one test that makes the loop panic on purpose.
    pub loop_panic_expected: std::sync::atomic::AtomicBool,
}

impl EventSink for Events {
    fn emit(&self, event: SessionEvent) {
        // The loop contains its own panics and ends the turn. In a test
        // that is a bug showing itself (a failed assertion in the loop),
        // and it must fail the test whatever else the test looks at.
        if let Event::TurnCompleted(completed) = &event.event
            && let Some(error) = &completed.error
            && error.code == "loop_panicked"
        {
            assert!(
                self.loop_panic_expected
                    .load(std::sync::atomic::Ordering::SeqCst),
                "the loop panicked: {}",
                error.message
            );
        }
        lock(&self.list).push(event);
        self.wake.notify_one();
    }
}

/// A short name for an event, for asserting on order.
pub fn label(event: &Event) -> String {
    match event {
        Event::TurnStarted(_) => "turn.started".to_owned(),
        Event::ItemStarted(started) => match &started.kind {
            ItemKind::AssistantMessage => "started:assistant".to_owned(),
            _ => format!("started:{}", started.tool.as_deref().unwrap_or("?")),
        },
        Event::ItemDelta(delta) => match &delta.delta {
            Delta::Text(_) => "delta:text".to_owned(),
            Delta::Reasoning(_) => "delta:reasoning".to_owned(),
            Delta::ToolCall(_) => "delta:tool_call".to_owned(),
            Delta::ToolArgs(_) => "delta:tool_args".to_owned(),
            Delta::ToolOutput(_) => "delta:tool_output".to_owned(),
            Delta::Unknown(_) => "delta:?".to_owned(),
        },
        Event::ItemReset(_) => "reset".to_owned(),
        Event::ItemCompleted(completed) => match &completed.item {
            Item::UserMessage(_) => "user".to_owned(),
            Item::AssistantMessage(_) => "assistant".to_owned(),
            Item::ToolResult(result) => {
                format!("result:{}:{}", result.tool, result.outcome.as_str())
            }
            Item::Notice(notice) => format!("note:{}", notice.source.as_str()),
            _ => "item:?".to_owned(),
        },
        Event::UsageUpdated(_) => "usage".to_owned(),
        Event::Notice(notice) => match notice {
            Notice::Retry(_) => "notice:retry".to_owned(),
            Notice::Compaction(compaction) => {
                format!("notice:compaction:{}", compaction.state.as_str())
            }
            Notice::Limit(_) => "notice:limit".to_owned(),
            Notice::SpeedFallback(_) => "notice:speed_fallback".to_owned(),
            Notice::SpeedCooldown(cooldown) => {
                format!("notice:speed_cooldown:{}", cooldown.state.as_str())
            }
            Notice::Info(_) => "notice:info".to_owned(),
            _ => "notice:?".to_owned(),
        },
        Event::TurnCompleted(completed) => format!("turn.completed:{}", completed.outcome.as_str()),
        Event::SessionIdle(_) => "idle".to_owned(),
        Event::SessionUpdated(_) => "updated".to_owned(),
        _ => "?".to_owned(),
    }
}

impl Events {
    /// Waits until `seen` holds of the events so far.
    pub async fn until(&self, seen: impl Fn(&[SessionEvent]) -> bool) {
        loop {
            if seen(&lock(&self.list)) {
                return;
            }
            self.wake.notified().await;
        }
    }

    /// Waits until `count` turns have ended.
    pub async fn turns(&self, count: usize) {
        self.until(|events| {
            events
                .iter()
                .filter(|event| matches!(event.event, Event::TurnCompleted(_)))
                .count()
                >= count
        })
        .await;
    }

    /// Waits until an event with this label has been seen `count` times.
    pub async fn seen(&self, wanted: &str, count: usize) {
        self.until(|events| {
            events
                .iter()
                .filter(|event| label(&event.event) == wanted)
                .count()
                >= count
        })
        .await;
    }

    pub fn all(&self) -> Vec<SessionEvent> {
        lock(&self.list).clone()
    }

    /// The labels of the events, without the two that say how things stand
    /// and not what happened (`usage.updated`, `session.updated`).
    pub fn labels(&self) -> Vec<String> {
        self.all()
            .iter()
            .map(|event| label(&event.event))
            .filter(|label| label != "usage" && label != "updated")
            .collect()
    }

    /// The labels of every event.
    pub fn every_label(&self) -> Vec<String> {
        self.all().iter().map(|event| label(&event.event)).collect()
    }

    /// The items that were completed, in order.
    pub fn items(&self) -> Vec<Item> {
        self.all()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::ItemCompleted(completed) => Some(completed.item),
                _ => None,
            })
            .collect()
    }

    /// How each turn ended, in order.
    pub fn ends(&self) -> Vec<TurnCompleted> {
        self.all()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::TurnCompleted(completed) => Some(completed),
                _ => None,
            })
            .collect()
    }

    /// The notices front ends were shown, in order.
    pub fn notices(&self) -> Vec<Notice> {
        self.all()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::Notice(notice) => Some(notice),
                _ => None,
            })
            .collect()
    }

    /// The position of the first event with this label.
    pub fn position(&self, wanted: &str) -> usize {
        self.every_label()
            .iter()
            .position(|label| label == wanted)
            .unwrap_or_else(|| panic!("no `{wanted}` in {:?}", self.every_label()))
    }
}

// ------------------------------------------------------------- transitions

/// Every change of state a session made. It can also interrupt the turn
/// the moment a chosen state is entered.
#[derive(Default)]
pub struct Seen {
    edges: Mutex<Vec<(TurnState, TurnState)>>,
    /// The state to interrupt at, how many entries of it to let pass
    /// first, and the session to interrupt.
    interrupt_at: Mutex<Option<(TurnState, usize, SessionHandle)>>,
}

impl Observer for Seen {
    fn transition(&self, _session: &SessionId, from: TurnState, to: TurnState) {
        lock(&self.edges).push((from, to));
        let fire = {
            let mut armed = lock(&self.interrupt_at);
            match armed.as_mut() {
                Some((state, skip, _)) if *state == to && *skip > 0 => {
                    *skip -= 1;
                    None
                }
                Some((state, _, _)) if *state == to => armed.take(),
                _ => None,
            }
        };
        if let Some((_, _, handle)) = fire {
            let _ = handle.interrupt();
        }
    }
}

impl Seen {
    /// The changes of state, in order.
    pub fn edges(&self) -> Vec<(TurnState, TurnState)> {
        lock(&self.edges).clone()
    }

    /// The states entered, in order, starting from idle.
    pub fn states(&self) -> Vec<TurnState> {
        let mut states = vec![TurnState::Idle];
        states.extend(self.edges().iter().map(|(_, to)| *to));
        states
    }

    /// Interrupts the running turn the next time `state` is entered.
    pub fn interrupt_at(&self, state: TurnState, handle: &SessionHandle) {
        self.interrupt_at_entry(state, 0, handle);
    }

    /// Interrupts the running turn when `state` is entered, after letting
    /// `skip` entries of it pass.
    pub fn interrupt_at_entry(&self, state: TurnState, skip: usize, handle: &SessionHandle) {
        *lock(&self.interrupt_at) = Some((state, skip, handle.clone()));
    }
}

// ------------------------------------------------------------------- tools

/// What the fake tools did, in order: `start read a`, `end read a`.
#[derive(Default)]
pub struct Log {
    lines: Mutex<Vec<String>>,
    wake: Notify,
}

impl Log {
    fn push(&self, line: String) {
        lock(&self.lines).push(line);
        self.wake.notify_one();
    }

    pub fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }

    /// Waits until the log has this line.
    pub async fn has(&self, wanted: &str) {
        loop {
            if lock(&self.lines).iter().any(|line| line == wanted) {
                return;
            }
            self.wake.notified().await;
        }
    }

    pub fn count(&self, wanted: &str) -> usize {
        lock(&self.lines)
            .iter()
            .filter(|line| *line == wanted)
            .count()
    }
}

/// Gates by name, made when first asked for. A fake tool waits at the gate
/// its `gate` argument names.
#[derive(Clone, Default)]
pub struct Gates {
    gates: Arc<Mutex<BTreeMap<String, Gate>>>,
}

impl Gates {
    pub fn get(&self, name: &str) -> Gate {
        lock(&self.gates)
            .entry(name.to_owned())
            .or_default()
            .clone()
    }

    pub fn open(&self, name: &str) {
        self.get(name).open();
    }
}

/// What kind of tool a [`Probe`] stands in for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Reads `path`: read-only.
    Read,
    /// Writes `path`: keyed by it.
    Write,
    /// Runs a command: exclusive.
    Shell,
    /// Touches nothing.
    Plain,
    /// Declares no effect and is scheduled to run alone: what a tool from
    /// outside looks like when its author said nothing.
    Solo,
    /// Panics when it runs.
    Panic,
}

/// A fake tool. Its arguments say what one call does:
///
/// - `path`: the path it reads or writes;
/// - `id`: the call's label in the log, in place of the path;
/// - `gate`: the name of a gate to wait at before it finishes;
/// - `output`: text to send to front ends while it runs;
/// - `stubborn`: wait at the gate even after the call is cancelled;
/// - `record`: note in the table of seen files, as `read` does, that the
///   path was shown;
/// - `fail`: end with an error result;
/// - `bad`: refuse the arguments when the call is planned.
pub struct Probe {
    spec: ToolSpec,
    kind: Kind,
    log: Arc<Log>,
    gates: Gates,
}

struct ProbeArgs {
    label: String,
    path: Option<cortex_base::AbsPath>,
    gate: Option<String>,
    output: Option<String>,
    stubborn: bool,
    record: bool,
    fail: bool,
}

impl Probe {
    pub fn new(name: &str, kind: Kind, log: &Arc<Log>, gates: &Gates) -> Self {
        let class = match kind {
            Kind::Read | Kind::Plain | Kind::Panic => ConcurrencyClass::ReadOnly,
            Kind::Write => ConcurrencyClass::Keyed,
            Kind::Shell | Kind::Solo => ConcurrencyClass::Exclusive,
        };
        Self {
            spec: ToolSpec::new(
                name,
                format!("The fake `{name}` tool."),
                json!({"type": "object"}),
            )
            .concurrency(class),
            kind,
            log: Arc::clone(log),
            gates: gates.clone(),
        }
    }
}

impl Tool for Probe {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        let text = |name: &str| args.get(name).and_then(Json::as_str).map(str::to_owned);
        if args.get("bad").is_some() {
            return Err(ToolError::invalid("`bad` is not an argument of this tool."));
        }
        let path = text("path")
            .map(|path| cx.paths.resolve(&path))
            .transpose()?;
        let label = text("id").or_else(|| text("path")).unwrap_or_default();
        let mut plan = Plan::new(
            format!("{} {label}", self.spec.name),
            ProbeArgs {
                label,
                path: path.as_ref().map(|path| path.abs().clone()),
                gate: text("gate"),
                output: text("output"),
                stubborn: args.get("stubborn").is_some(),
                record: args.get("record").is_some(),
                fail: args.get("fail").is_some(),
            },
        );
        match (self.kind, path) {
            (Kind::Read, Some(path)) => plan = plan.with_effect(Effect::Read(path)),
            (Kind::Write, Some(path)) => {
                plan = plan
                    .with_concurrency(Concurrency::Keyed(vec![path.abs().clone()]))
                    .with_effect(Effect::Write(path));
            }
            (Kind::Solo, _) => plan = plan.with_concurrency(Concurrency::Exclusive),
            (Kind::Shell, _) => {
                plan = plan
                    .with_concurrency(Concurrency::Exclusive)
                    .with_effect(Effect::Exec {
                        command: "probe".to_owned(),
                        cwd: cx.paths.resolve(".")?,
                    });
            }
            _ => {}
        }
        Ok(plan)
    }

    fn run<'a>(
        &'a self,
        plan: Plan,
        cx: &'a RunCx,
        out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let name = &self.spec.name;
            let args: ProbeArgs = match plan.into_args() {
                Ok(args) => args,
                Err(error) => return error.into_result(),
            };
            let label = &args.label;
            self.log.push(format!("start {name} {label}"));
            if let Some(output) = &args.output {
                out.output(output);
            }
            if let Some(gate) = &args.gate {
                let opened = self.gates.get(gate).opened();
                if args.stubborn {
                    opened.await;
                } else if cx.cancel.race(opened).await.is_none() {
                    self.log.push(format!("cancelled {name} {label}"));
                    return ToolResult::interrupted(format!("{name} {label} stopped part-way"));
                }
            }
            assert!(self.kind != Kind::Panic, "the probe was told to panic");
            if let (true, Some(path)) = (args.record, &args.path) {
                let snapshot = cortex_tools::FileSnapshot::of_content(b"one line\n", None);
                let shown = cortex_tools::Shown {
                    hash: snapshot.hash,
                    lines: cortex_tools::LineRanges::of(1, 1),
                    total_lines: 1,
                    turn: cx.turn,
                    kept: false,
                };
                cx.files.record_read(path, snapshot, shown);
            }
            self.log.push(format!("end {name} {label}"));
            if args.fail {
                ToolResult::error(format!("{name} {label} failed"))
            } else {
                ToolResult::success(format!("{name} {label} ok"))
            }
        })
    }
}

// ------------------------------------------------------------------ policy

/// A policy a test scripts by tool name.
#[derive(Default)]
pub struct ScriptedPolicy {
    decisions: Mutex<BTreeMap<String, Decision>>,
    /// The answers to come for each tool's questions, in order. With none
    /// left the answer is to allow.
    answers: Mutex<BTreeMap<String, VecDeque<Verdict>>>,
    /// The calls it was shown to decide, by their plan's summary, in order.
    decided: Mutex<Vec<String>>,
    /// A session to interrupt at the moment a question is answered, so
    /// that the answer is on its way when the loop sees the interrupt.
    interrupts: Mutex<Option<SessionHandle>>,
    /// The tools whose question, or whose decision, panics.
    panics_asked: Mutex<Vec<String>>,
    panics_deciding: Mutex<Vec<String>>,
    /// The calls that were asked about, by their plan's summary, in order.
    asked: Mutex<Vec<String>>,
    /// The questions that were dropped before they were answered.
    withdrawn: Arc<Mutex<Vec<String>>>,
    /// Each question waits at the gate named after its tool, when one is
    /// set.
    wait: Mutex<Option<Gates>>,
    wake: Notify,
}

impl ScriptedPolicy {
    pub fn decide_for(&self, tool: &str, decision: Decision) {
        lock(&self.decisions).insert(tool.to_owned(), decision);
    }

    /// Adds the answer to the next question about `tool` that has none yet.
    pub fn answer_for(&self, tool: &str, verdict: Verdict) {
        lock(&self.answers)
            .entry(tool.to_owned())
            .or_default()
            .push_back(verdict);
    }

    /// Makes every question wait at the gate `ask <tool>`.
    pub fn wait_at(&self, gates: &Gates) {
        *lock(&self.wait) = Some(gates.clone());
    }

    pub fn asked(&self) -> Vec<String> {
        lock(&self.asked).clone()
    }

    /// Interrupts `session` in the same step as the next answer is given.
    pub fn interrupt_as_it_answers(&self, session: &SessionHandle) {
        *lock(&self.interrupts) = Some(session.clone());
    }

    /// Makes the question about `tool` panic.
    pub fn panic_when_asked(&self, tool: &str) {
        lock(&self.panics_asked).push(tool.to_owned());
    }

    /// Makes the decision about `tool` panic.
    pub fn panic_when_deciding(&self, tool: &str) {
        lock(&self.panics_deciding).push(tool.to_owned());
    }

    /// Waits until `count` calls have been planned and shown to the policy,
    /// and the loop has had the chance to start whatever it would start.
    pub async fn decisions(&self, count: usize) {
        loop {
            if lock(&self.decided).len() >= count {
                break;
            }
            self.wake.notified().await;
        }
        // The decision is made in the task that planned the call, which
        // then reports to the loop, which then starts the call, which logs
        // when it is first polled: a few turns of the runtime.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    pub fn withdrawn(&self) -> Vec<String> {
        lock(&self.withdrawn).clone()
    }

    /// Waits until `count` questions have been put.
    pub async fn questions(&self, count: usize) {
        loop {
            if lock(&self.asked).len() >= count {
                return;
            }
            self.wake.notified().await;
        }
    }
}

/// Notes a question that is dropped while it is still open.
struct OpenQuestion {
    summary: Option<String>,
    withdrawn: Arc<Mutex<Vec<String>>>,
}

impl Drop for OpenQuestion {
    fn drop(&mut self) {
        if let Some(summary) = self.summary.take() {
            lock(&self.withdrawn).push(summary);
        }
    }
}

impl Policy for ScriptedPolicy {
    fn decide(&self, call: &PlannedCall<'_>) -> Decision {
        lock(&self.decided).push(call.plan.summary.clone());
        self.wake.notify_one();
        assert!(
            !lock(&self.panics_deciding).contains(&call.spec.name),
            "the policy was told to panic deciding"
        );
        lock(&self.decisions)
            .get(&call.spec.name)
            .cloned()
            .unwrap_or(Decision::Allow)
    }

    fn ask<'a>(&'a self, call: PlannedCall<'a>) -> BoxFuture<'a, Verdict> {
        Box::pin(async move {
            let tool = call.spec.name.clone();
            lock(&self.asked).push(call.plan.summary.clone());
            self.wake.notify_one();
            assert!(
                !lock(&self.panics_asked).contains(&tool),
                "the policy was told to panic asking"
            );
            let mut open = OpenQuestion {
                summary: Some(call.plan.summary.clone()),
                withdrawn: Arc::clone(&self.withdrawn),
            };
            let gate = lock(&self.wait)
                .as_ref()
                .map(|gates| gates.get(&format!("ask {tool}")));
            if let Some(gate) = gate {
                gate.opened().await;
            }
            open.summary = None;
            if let Some(session) = lock(&self.interrupts).take() {
                let _ = session.interrupt();
            }
            lock(&self.answers)
                .get_mut(&tool)
                .and_then(VecDeque::pop_front)
                .unwrap_or(Verdict::Allow)
        })
    }
}

// ----------------------------------------------------------------- context

/// What a test tells the context to do.
#[derive(Default)]
pub struct ContextScript {
    /// Whether the context says it is over its budget. A compaction that
    /// succeeds clears it.
    pub over_budget: Mutex<bool>,
    /// What the next compactions return. Empty: they fail, as the stub's
    /// do.
    pub compactions: Mutex<VecDeque<Result<(), ContextError>>>,
    /// A gate each compaction waits at.
    pub gate: Mutex<Option<Gate>>,
    pub compacted: Mutex<u32>,
}

/// The stub context, with compaction a test can script.
pub struct ScriptedContext {
    inner: StubContext,
    script: Arc<ContextScript>,
}

impl Context for ScriptedContext {
    fn load(&mut self, items: &[Item]) {
        self.inner.load(items);
    }

    fn push(&mut self, item: &Item) {
        self.inner.push(item);
    }

    fn assemble(&self) -> Assembled {
        self.inner.assemble()
    }

    fn needs_compaction(&self) -> bool {
        *lock(&self.script.over_budget)
    }

    fn compact<'a>(
        &'a mut self,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<(), ContextError>> {
        Box::pin(async move {
            let gate = lock(&self.script.gate).clone();
            if let Some(gate) = gate
                && cancel.race(gate.opened()).await.is_none()
            {
                return Err(ContextError::new("cancelled"));
            }
            *lock(&self.script.compacted) += 1;
            let outcome = lock(&self.script.compactions)
                .pop_front()
                .unwrap_or_else(|| {
                    Err(ContextError::new("The conversation cannot be made to fit."))
                });
            if outcome.is_ok() {
                *lock(&self.script.over_budget) = false;
            }
            outcome
        })
    }
}

// ---------------------------------------------------------------- verifier

/// A verifier that returns what a test scripted, round by round.
#[derive(Default)]
pub struct ScriptedVerifier {
    pub outcomes: Mutex<VecDeque<VerifyOutcome>>,
    /// The rounds that ran: the round number and how many files had been
    /// written.
    pub rounds: Mutex<Vec<(u32, usize)>>,
    pub gate: Mutex<Option<Gate>>,
}

impl Verifier for ScriptedVerifier {
    fn verify<'a>(&'a self, cx: VerifyCx<'a>) -> BoxFuture<'a, VerifyOutcome> {
        Box::pin(async move {
            lock(&self.rounds).push((cx.round, cx.written.len()));
            let gate = lock(&self.gate).clone();
            if let Some(gate) = gate
                && cx.cancel.race(gate.opened()).await.is_none()
            {
                return VerifyOutcome::Skipped;
            }
            lock(&self.outcomes)
                .pop_front()
                .unwrap_or(VerifyOutcome::Skipped)
        })
    }
}

// ------------------------------------------------------------------- stats

#[derive(Default)]
pub struct StatLog {
    events: Mutex<Vec<StatEvent>>,
}

impl Stats for StatLog {
    fn record(&self, event: StatEvent) {
        lock(&self.events).push(event);
    }
}

impl StatLog {
    pub fn all(&self) -> Vec<StatEvent> {
        lock(&self.events).clone()
    }
}

// --------------------------------------------------------------------- rig

/// A directory of the test's own, made empty.
pub fn test_dir(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cortex-agent-it")
        .join(test);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear the test's directory");
    }
    std::fs::create_dir_all(&dir).expect("create the test's directory");
    dir
}

/// What every session of a test may share: the clock, the provider's
/// script, the tools' log and gates.
pub struct World {
    pub runtime: tokio::runtime::Runtime,
    pub clock: FakeClock,
    pub provider: ScriptedProvider,
    pub log: Arc<Log>,
    pub gates: Gates,
    pub cooldowns: SpeedCooldowns,
    pub dir: PathBuf,
}

impl World {
    pub fn new(test: &str, responses: impl IntoIterator<Item = Response>) -> Self {
        let clock = FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap();
        let shared: SharedClock = Arc::new(clock.clone());
        Self {
            runtime: tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a runtime"),
            cooldowns: SpeedCooldowns::new(shared),
            clock,
            provider: ScriptedProvider::new(responses),
            log: Arc::default(),
            gates: Gates::default(),
            dir: test_dir(test),
        }
    }

    pub fn shared_clock(&self) -> SharedClock {
        Arc::new(self.clock.clone())
    }

    /// The fake tools: `read` and `peek` (read-only), `write` (keyed by
    /// path), `shell` (exclusive), `plain` (touches nothing), `solo`
    /// (declares nothing and runs alone) and `panic`.
    pub fn toolbox(&self) -> Toolbox {
        let probe = |name: &str, kind: Kind| Probe::new(name, kind, &self.log, &self.gates);
        Toolbox::new()
            .with(probe("read", Kind::Read))
            .with(probe("peek", Kind::Read))
            .with(probe("write", Kind::Write))
            .with(probe("shell", Kind::Shell))
            .with(probe("plain", Kind::Plain))
            .with(probe("solo", Kind::Solo))
            .with(probe("panic", Kind::Panic))
    }

    /// Runs a future on the test's runtime, with the sessions.
    pub fn run<T>(&self, future: impl Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }
}

/// A panic in a session's task would leave the test waiting for an event
/// that never comes, until the runner's time limit. This ends the test at
/// once, with the panic's message already printed.
struct AbortIfItPanics<F>(std::pin::Pin<Box<F>>);

impl<F: Future<Output = ()>> Future for AbortIfItPanics<F> {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let inner = self.0.as_mut();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.poll(context))) {
            Ok(poll) => poll,
            Err(_) => std::process::abort(),
        }
    }
}

/// How one session of a test is put together.
pub struct Setup {
    pub id: String,
    pub config: LoopConfig,
    pub ledger: Option<Ledger>,
    pub speeds: SpeedSettings,
    pub model: RoleModel,
    /// The model of the `fast` utility role, when the session has one.
    pub utility: Option<RoleModel>,
    pub store: Option<Box<dyn SessionStore>>,
    pub memory: MemorySessionStore,
    pub toolbox: Option<Toolbox>,
    pub context: Arc<ContextScript>,
    /// A verifier in place of the scripted one.
    pub verifier: Option<Arc<dyn Verifier>>,
}

/// One session, with everything it does remembered.
pub struct Rig {
    pub handle: SessionHandle,
    pub events: Arc<Events>,
    pub seen: Arc<Seen>,
    pub stats: Arc<StatLog>,
    pub policy: Arc<ScriptedPolicy>,
    pub verifier: Arc<ScriptedVerifier>,
    pub context: Arc<ContextScript>,
    pub store: MemorySessionStore,
    pub workspace: Workspace,
}

impl Setup {
    pub fn new(world: &World) -> Self {
        Self {
            id: "s_1".to_owned(),
            config: LoopConfig::default(),
            ledger: None,
            speeds: SpeedSettings::default(),
            model: RoleModel::new(Arc::new(world.provider.clone()), fixtures::model()),
            utility: None,
            store: None,
            memory: MemorySessionStore::new(),
            toolbox: None,
            context: Arc::default(),
            verifier: None,
        }
    }

    /// Opens the session and starts its loop on the world's runtime.
    pub fn start(self, world: &World) -> Rig {
        let root = world.dir.join(&self.id);
        std::fs::create_dir_all(&root).expect("create the workspace");
        let paths = Arc::new(PathResolver::new(&root, PathOptions::new()).expect("a workspace"));
        let workspace = Workspace::in_memory(paths);
        let toolbox = self.toolbox.unwrap_or_else(|| world.toolbox());
        let clock = world.shared_clock();
        let events = Arc::new(Events::default());
        let seen = Arc::new(Seen::default());
        let stats = Arc::new(StatLog::default());
        let policy = Arc::new(ScriptedPolicy::default());
        let verifier = Arc::new(ScriptedVerifier::default());
        let context = ScriptedContext {
            inner: StubContext::new("You are a test.", &toolbox.specs()),
            script: Arc::clone(&self.context),
        };
        let parts = SessionParts {
            info: SessionInfo::new(SessionId::new(self.id), "/work/test", &clock),
            models: match self.utility {
                Some(utility) => Models::new(self.model, "test-1")
                    .with_role(cortex_protocol::model::ModelRole::Fast, utility),
                None => Models::new(self.model, "test-1"),
            },
            speeds: self.speeds,
            tools: Arc::new(toolbox),
            policy: Arc::clone(&policy) as Arc<dyn Policy>,
            store: self.store.unwrap_or_else(|| Box::new(self.memory.clone())),
            context: Box::new(context),
            events: Arc::clone(&events) as Arc<dyn EventSink>,
            stats: Arc::clone(&stats) as Arc<dyn Stats>,
            verifier: self
                .verifier
                .unwrap_or_else(|| Arc::clone(&verifier) as Arc<dyn Verifier>),
            clock: Arc::clone(&clock),
            cooldowns: world.cooldowns.clone(),
            ledger: self.ledger.unwrap_or_else(|| Ledger::new(clock)),
            workspace: workspace.clone(),
            config: self.config,
            cancel: CancelToken::new(),
            observer: Some(Arc::clone(&seen) as Arc<dyn Observer>),
        };
        let (session, handle) = world
            .runtime
            .block_on(Session::open(parts))
            .expect("the session opens");
        world
            .runtime
            .spawn(AbortIfItPanics(Box::pin(session.run())));
        Rig {
            handle,
            events,
            seen,
            stats,
            policy,
            verifier,
            context: self.context,
            store: self.memory,
            workspace,
        }
    }
}

impl Rig {
    /// A world with one session, at its defaults.
    pub fn one(test: &str, responses: impl IntoIterator<Item = Response>) -> (World, Self) {
        let world = World::new(test, responses);
        let rig = Setup::new(&world).start(&world);
        (world, rig)
    }

    /// Sends a prompt that starts a turn.
    pub fn prompt(&self, text: &str) -> TurnStartResult {
        self.handle
            .prompt(vec![Part::text(text)], None)
            .map_err(|error| *error)
            .expect("the session is idle")
    }

    /// Sends a prompt, queues another behind it while the first turn is
    /// still at its first request, and waits for the first turn to end.
    pub fn queue_after_start(&self, world: &World, queued: &str) {
        world.run(async {
            self.prompt("go");
            self.queue(queued);
            self.events.turns(1).await;
        });
    }

    pub fn steer(&self, text: &str) -> TurnStartResult {
        self.handle
            .prompt(vec![Part::text(text)], Some(Delivery::Steer))
            .map_err(|error| *error)
            .expect("a turn is running")
    }

    pub fn queue(&self, text: &str) -> TurnStartResult {
        self.handle
            .prompt(vec![Part::text(text)], Some(Delivery::Queue))
            .map_err(|error| *error)
            .expect("a turn is running")
    }
}

// ----------------------------------------------------------------- history

/// The text of a message's text parts.
pub fn text_of(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Checks a conversation the way a provider does before it accepts a
/// request: it begins with the user, no message is empty, every tool call
/// of an assistant message has exactly one result, in the message right
/// after it and in call order, and no result is without its call.
pub fn assert_valid_history(messages: &[Message]) {
    assert!(!messages.is_empty(), "a request has messages");
    assert_eq!(messages[0].role, Role::User, "the user speaks first");
    for (index, message) in messages.iter().enumerate() {
        assert!(
            !message.parts.is_empty(),
            "message {index} is empty: {messages:#?}"
        );
        let calls: Vec<&str> = message
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::ToolCall(call) => Some(call.id.as_str()),
                _ => None,
            })
            .collect();
        match message.role {
            Role::Assistant if !calls.is_empty() => {
                let results: Vec<&str> = messages
                    .get(index + 1)
                    .filter(|next| next.role == Role::Tool)
                    .map(|next| {
                        next.parts
                            .iter()
                            .filter_map(|part| match part {
                                Part::ToolResult(result) => Some(result.call_id.as_str()),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(
                    results, calls,
                    "the calls of message {index} and the results after it: {messages:#?}"
                );
            }
            Role::Tool => {
                let after_calls = index > 0
                    && messages[index - 1].role == Role::Assistant
                    && messages[index - 1]
                        .parts
                        .iter()
                        .any(|part| matches!(part, Part::ToolCall(_)));
                assert!(
                    after_calls,
                    "results at {index} follow no call: {messages:#?}"
                );
                assert!(
                    message
                        .parts
                        .iter()
                        .all(|part| matches!(part, Part::ToolResult(_))),
                    "a tool message holds results only"
                );
            }
            _ => assert!(calls.is_empty(), "only the assistant calls tools"),
        }
    }
}

/// The last request the provider received.
pub fn last_request(provider: &ScriptedProvider) -> ModelRequest {
    provider.requests().pop().expect("a request was sent")
}

/// A response that calls one fake tool and asks for the round to run.
pub fn calls(calls: &[(&str, &str, Json)]) -> Response {
    let mut response = Response::new();
    for (id, tool, arguments) in calls {
        response = response.tool_call(id, tool, arguments);
    }
    response.end(StopReason::ToolCalls)
}

/// A response that only says something.
pub fn says(text: &str) -> Response {
    Response::new().text(text).end(StopReason::End)
}
