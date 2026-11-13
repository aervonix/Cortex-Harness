//! Headless mode: a prompt run to completion, with a printer for a front
//! end (`docs/architecture/10-headless.md`).
//!
//! Three threads:
//!
//! - **The core thread** runs a current-thread Tokio runtime with its timer
//!   and its I/O driver on: the provider layer's and the process runner's
//!   waits need the first, and the HTTP client's sockets the second.
//!   It starts the core, chooses the session, starts the turn, reads the
//!   client's queue and hands each line to the writer.
//! - **The writer thread** writes standard output, a line at a time, and
//!   flushes each. A reader that is slow holds the writer up, never the
//!   core: the core's queue merges and drops deltas for a client that is
//!   behind, and the channel to the writer is bounded.
//! - **The main thread** watches for signals ([`crate::signals`]) until the
//!   core thread has finished.
//!
//! Standard output carries the result and nothing else. Diagnostics go to
//! standard error, through the log's stderr sink and the notices.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use cortex_base::log::{self, FileOptions, Filter, LogHandle, LogOptions};
use cortex_base::{CancelToken, SecretFilter, SystemClock};
use cortex_config::{Dirs, Env, Override, StateDir};
use cortex_core::{ClientInfo, Core, CoreOptions, OpError, Outgoing, TokioTimer, parse_rule};
use cortex_protocol::event::{Event, TurnOutcome};
use cortex_protocol::headless::{HeadlessResult, PermissionDenial};
use cortex_protocol::lease::RuleAction;
use cortex_protocol::message::Part;
use cortex_protocol::methods::{
    Op, OpResult, SessionForkParams, SessionListParams, SessionNewParams, SessionRef,
    TurnInterruptParams, TurnStartParams,
};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::{SessionSettings, SessionState};
use cortex_protocol::{ErrorData, ErrorKind, ModelId, SessionId};
use tokio::sync::mpsc;
use tracing::metadata::LevelFilter;

use crate::args::{Choice, Format, Run};
use crate::exit::{self, Signal};
use crate::report::{self, Collector};
use crate::signals::{self, Interrupt};

/// The most standard input that is read as part of the prompt.
const STDIN_LIMIT: u64 = 32 * 1024 * 1024;
/// Lines that may wait for the writer before the core thread waits.
const WRITER_QUEUE: usize = 64;

/// What the core thread needs from the command line.
struct Plan {
    run: Run,
    prompt: String,
    name: &'static str,
    workspace: PathBuf,
    started: Instant,
    guard: Option<(PathBuf, Vec<OsString>)>,
    log: Option<Arc<LogHandle>>,
    env: Env,
}

/// Runs a headless prompt. Returns the exit code.
pub(crate) fn run(run: Run, name: &'static str, started: Instant) -> u8 {
    let env = Env::from_process();
    let log = start_log(&run, &env, name);
    let format = run.format;
    let fail = |kind: ErrorKind, code: &str, message: String| {
        let error = ErrorData::new(kind, code, message);
        finish_early(format, run.quiet, name, &error, started)
    };
    let prompt = match prompt(run.prompt.as_deref()) {
        Ok(Some(prompt)) => prompt,
        Ok(None) => {
            return fail(
                ErrorKind::Usage,
                "no_prompt",
                format!(
                    "no prompt: give it after -p, or on standard input (`{name} -p \"…\"`, or \
                     `… | {name} -p`)"
                ),
            );
        }
        Err(error) => {
            return fail(
                ErrorKind::Usage,
                "unreadable_prompt",
                format!("standard input could not be read: {error}"),
            );
        }
    };
    let workspace = match std::env::current_dir() {
        Ok(directory) => directory,
        Err(error) => {
            return fail(
                ErrorKind::Usage,
                "no_working_directory",
                format!("the working directory could not be read: {error}"),
            );
        }
    };
    // The harness's own executable, under the hidden subcommand: the guard
    // of process trees. Without a path to itself, commands run unguarded.
    let guard = std::env::current_exe()
        .ok()
        .map(|exe| (exe, vec![OsString::from(crate::args::GUARD)]));

    let (lines, written) = mpsc::channel::<String>(WRITER_QUEUE);
    let (writer_done, writer_finished) = tokio::sync::oneshot::channel::<()>();
    let writer = std::thread::Builder::new()
        .name("cortex-writer".to_owned())
        .spawn(move || {
            write_lines(written);
            let _ = writer_done.send(());
        });
    let Ok(writer) = writer else {
        return fail(
            ErrorKind::Internal,
            "no_thread",
            "the writer thread could not be started".to_owned(),
        );
    };
    let (interrupts, interrupted) = mpsc::unbounded_channel::<Interrupt>();
    let (done, finished) = tokio::sync::oneshot::channel::<u8>();
    let handle = log.clone();
    let quiet = run.quiet;
    let plan = Plan {
        run,
        prompt,
        name,
        workspace,
        started,
        guard,
        log,
        env,
    };
    let core_thread = std::thread::Builder::new()
        .name("cortex-core".to_owned())
        .spawn(move || {
            let code = core_thread(plan, &lines, interrupted);
            drop(lines);
            let _ = done.send(code);
            code
        });
    let Ok(core_thread) = core_thread else {
        let error = ErrorData::new(
            ErrorKind::Internal,
            "no_thread",
            "the core thread could not be started",
        );
        return finish_early(format, quiet, name, &error, started);
    };

    // The main thread watches for signals until the run is over: the core
    // thread is done and the writer has written everything. Once the core
    // thread is done there is nothing left to interrupt, so a signal then
    // ends the process at once (a reader that takes nothing more would
    // otherwise hold it).
    let core_done = Arc::new(AtomicBool::new(false));
    let watched = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|runtime| {
            let core_done = Arc::clone(&core_done);
            runtime.block_on(async move {
                let watch = signals::watch(interrupts, Arc::clone(&core_done));
                race_done(finished, writer_finished, &core_done, watch).await;
            });
        });
    if let Err(error) = watched {
        tracing::warn!(%error, "signals are not watched: their runtime did not start");
    }
    let code = core_thread.join();
    let _ = writer.join();
    if let Some(handle) = handle {
        handle.flush();
    }
    match code {
        Ok(code) => code,
        Err(_) => {
            // A panic on the core thread: still a record, and the code that
            // says the harness itself failed.
            let error = ErrorData::new(
                ErrorKind::Internal,
                "core_panicked",
                "the core thread failed; the log file has the details",
            );
            finish_early(format, quiet, name, &error, started)
        }
    }
}

/// Waits for the core thread and then the writer to be done, watching for
/// signals meanwhile, and marks the core done when it is. Watching may end
/// first (no signals can be watched): the wait goes on.
async fn race_done(
    mut finished: tokio::sync::oneshot::Receiver<u8>,
    mut writer_finished: tokio::sync::oneshot::Receiver<()>,
    core_done: &AtomicBool,
    watch: impl std::future::Future<Output = ()>,
) {
    let mut watch = std::pin::pin!(watch);
    let mut watching = true;
    let (mut core, mut writer) = (false, false);
    std::future::poll_fn(|context| {
        if !core && std::pin::Pin::new(&mut finished).poll(context).is_ready() {
            core = true;
            core_done.store(true, Ordering::SeqCst);
        }
        if !writer
            && std::pin::Pin::new(&mut writer_finished)
                .poll(context)
                .is_ready()
        {
            writer = true;
        }
        if core && writer {
            return std::task::Poll::Ready(());
        }
        if watching && watch.as_mut().poll(context).is_ready() {
            watching = false;
        }
        std::task::Poll::Pending
    })
    .await;
}

/// The core thread: a runtime, the core, and the run. Returns the exit code.
fn core_thread(
    plan: Plan,
    lines: &mpsc::Sender<String>,
    interrupted: mpsc::UnboundedReceiver<Interrupt>,
) -> u8 {
    // The timer for the provider layer's and the process runner's waits,
    // and the I/O driver for the HTTP client's sockets.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "the core thread's runtime did not start");
            return exit::INTERNAL;
        }
    };
    runtime.block_on(drive(plan, lines, interrupted))
}

/// What a run in progress has to hand.
struct Front<'a> {
    plan: &'a Plan,
    lines: &'a mpsc::Sender<String>,
    interrupted: mpsc::UnboundedReceiver<Interrupt>,
    /// The first signal, which decides the exit code of an interrupted run.
    signal: Option<Signal>,
    /// Whether to print a line for each tool call on standard error.
    progress: bool,
    /// The turn a signal interrupts, once it has been asked for.
    target: Option<(Core, cortex_core::ClientId, SessionId)>,
}

impl Front<'_> {
    /// Hands a line to the writer. While the writer has no room (a reader
    /// that is slow, or stopped), signals are still taken: each interrupts
    /// the turn as it would anywhere else.
    async fn write(&mut self, line: String) {
        if line.is_empty() {
            return;
        }
        let mut send = std::pin::pin!(self.lines.send(line));
        loop {
            let mut signals_open = true;
            let next = std::future::poll_fn(|context| {
                if let std::task::Poll::Ready(sent) = send.as_mut().poll(context) {
                    // A writer that has gone (standard output closed) takes
                    // nothing more, and the run goes on to its end.
                    let _ = sent;
                    return std::task::Poll::Ready(None);
                }
                if signals_open {
                    match self.interrupted.poll_recv(context) {
                        std::task::Poll::Ready(Some(interrupt)) => {
                            return std::task::Poll::Ready(Some(interrupt));
                        }
                        std::task::Poll::Ready(None) => signals_open = false,
                        std::task::Poll::Pending => {}
                    }
                }
                std::task::Poll::Pending
            })
            .await;
            match next {
                None => return,
                Some(interrupt) => self.interrupt(interrupt).await,
            }
        }
    }

    /// Interrupts the turn, or, before there is one, remembers the signal
    /// so that none is started.
    async fn interrupt(&mut self, interrupt: Interrupt) {
        self.signal.get_or_insert(interrupt.signal);
        if let Some((core, client, session)) = &self.target {
            let _ = core
                .handle(
                    *client,
                    Op::TurnInterrupt(TurnInterruptParams {
                        session_id: session.clone(),
                        now: interrupt.now,
                    }),
                )
                .await;
        }
    }

    fn say(&self, text: &str) {
        if !self.plan.run.quiet {
            eprintln!("{}: {}", self.plan.name, crate::visible(text));
        }
    }

    /// Takes the signals that have arrived. Says whether there was one.
    fn signalled(&mut self) -> bool {
        while let Ok(interrupt) = self.interrupted.try_recv() {
            self.signal.get_or_insert(interrupt.signal);
        }
        self.signal.is_some()
    }
}

async fn drive(
    plan: Plan,
    lines: &mpsc::Sender<String>,
    interrupted: mpsc::UnboundedReceiver<Interrupt>,
) -> u8 {
    let run = &plan.run;
    let mut front = Front {
        plan: &plan,
        lines,
        interrupted,
        signal: None,
        progress: run.format == Format::Text && !run.quiet && io::stderr().is_terminal(),
        target: None,
    };
    let core = match start_core(&plan) {
        Ok(core) => core,
        Err(error) => return failed(&mut front, *error).await,
    };
    let code = in_core(&core, &mut front).await;
    core.shutdown().await;
    code
}

/// Everything after the core started: the session, the turn and the
/// result.
async fn in_core(core: &Core, front: &mut Front<'_>) -> u8 {
    let run = &front.plan.run;
    let mut info = ClientInfo::headless();
    info.capabilities.deltas = run.include_deltas && run.format == Format::Jsonl;
    let client = core.connect(info);

    let state = match session(core, client.id, front.plan).await {
        Ok(state) => state,
        Err(error) => return failed(front, *error).await,
    };
    let session = state.session_id.clone();
    if run.format == Format::Jsonl {
        let model = state
            .roles
            .get(&ModelRole::Main)
            .map_or_else(|| ModelId::from(""), |role| role.model.clone());
        front
            .write(report::start_line(
                session.clone(),
                model,
                state.workspace.clone(),
            ))
            .await;
    }
    let mut collector = Collector::new(session.clone());

    // A signal that came while the session was being prepared stops the
    // run before anything is sent.
    if front.signalled() {
        let error = interrupted_error();
        return finish(
            core,
            front,
            &collector,
            Some((TurnOutcome::Interrupted, error)),
        )
        .await;
    }
    let started = core
        .handle(
            client.id,
            Op::TurnStart(TurnStartParams {
                session_id: session.clone(),
                input: vec![Part::text(front.plan.prompt.clone())],
                delivery: None,
            }),
        )
        .await;
    if let Err(error) = started {
        let outcome = exit::outcome_for_kind(&error.kind);
        return finish(core, front, &collector, Some((outcome, *error))).await;
    }
    front.target = Some((core.clone(), client.id, session.clone()));

    loop {
        let next = next_or_signal(&client.queue, &mut front.interrupted).await;
        match next {
            Next::Signal(interrupt) => front.interrupt(interrupt).await,
            Next::Outgoing(Outgoing::Event(event)) => {
                let ended = collector.observe(&event);
                if front.plan.run.format == Format::Jsonl {
                    front.write(report::event_line(&event)).await;
                }
                tell(front, &event.event);
                if ended {
                    break;
                }
            }
            // Headless mode answers nothing, and declared so: no request
            // is sent to it.
            Next::Outgoing(Outgoing::Request { .. }) => {}
            Next::Closed => {
                let error = ErrorData::new(
                    ErrorKind::Internal,
                    "client_closed",
                    "the core closed this run's connection before the turn ended",
                );
                return finish(core, front, &collector, Some((TurnOutcome::Failed, error))).await;
            }
        }
    }
    let code = finish(core, front, &collector, None).await;
    let _ = core
        .handle(
            client.id,
            Op::SessionClose(SessionRef {
                session_id: session,
            }),
        )
        .await;
    code
}

/// Writes the result in the run's format, says what went wrong on standard
/// error, and returns the exit code that agrees with the outcome.
async fn finish(
    core: &Core,
    front: &mut Front<'_>,
    collector: &Collector,
    failure: Option<(TurnOutcome, ErrorData)>,
) -> u8 {
    let denials = denials(core, collector);
    let result = collector.result(failure, denials, elapsed(front.plan.started));
    let code = exit::for_outcome(
        &result.outcome,
        result.error.as_ref().map(|error| &error.kind),
        front.signal,
    );
    write_result(front, result).await;
    code
}

fn denials(core: &Core, collector: &Collector) -> Vec<PermissionDenial> {
    let Some(session) = collector.session() else {
        return Vec::new();
    };
    core.permission_denials(session)
        .into_iter()
        .map(|denial| PermissionDenial {
            turn_id: denial.turn_id,
            call_id: denial.call_id,
            tool: denial.tool,
            summary: denial.summary,
            reason: denial.reason,
        })
        .collect()
}

async fn write_result(front: &mut Front<'_>, result: HeadlessResult) {
    if let Some(error) = &result.error {
        front.say(&format!(
            "{} ({}: {})",
            error.message, error.kind, error.code
        ));
    }
    for denial in &result.permission_denials {
        front.say(&format!("denied: {} ({})", denial.summary, denial.reason));
    }
    let line = match front.plan.run.format {
        Format::Text => report::text_output(&result),
        Format::Json | Format::Jsonl => report::result_line(result),
    };
    front.write(line).await;
}

/// A run that failed before it had a session.
async fn failed(front: &mut Front<'_>, error: ErrorData) -> u8 {
    let collector = Collector::default();
    let outcome = exit::outcome_for_kind(&error.kind);
    let result = collector.result(
        Some((outcome, error)),
        Vec::new(),
        elapsed(front.plan.started),
    );
    let code = exit::for_outcome(
        &result.outcome,
        result.error.as_ref().map(|error| &error.kind),
        front.signal,
    );
    write_result(front, result).await;
    code
}

/// A run that failed before its threads were started: its record is
/// written from the main thread.
fn finish_early(
    format: Format,
    quiet: bool,
    name: &str,
    error: &ErrorData,
    started: Instant,
) -> u8 {
    if !quiet {
        eprintln!("{name}: {}", crate::visible(&error.message));
    }
    let outcome = exit::outcome_for_kind(&error.kind);
    let result =
        Collector::default().result(Some((outcome, error.clone())), Vec::new(), elapsed(started));
    let code = exit::for_outcome(&result.outcome, Some(&error.kind), None);
    if format != Format::Text {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(report::result_line(result).as_bytes());
        let _ = stdout.flush();
    }
    code
}

/// What to tell the person on standard error as an event passes: the
/// notices, and in `text` format a line for each tool call when standard
/// error is a terminal.
fn tell(front: &Front<'_>, event: &Event) {
    match event {
        Event::Notice(notice) => {
            if let Some(message) = notice.message() {
                front.say(message);
            }
        }
        Event::ItemStarted(started) if front.progress => {
            if let Some(tool) = &started.tool {
                front.say(&format!("· {tool}"));
            }
        }
        _ => {}
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "one value at a time, moved straight out: a box would be an allocation per event"
)]
enum Next {
    Outgoing(Outgoing),
    Signal(Interrupt),
    Closed,
}

/// The next thing for the client, or a signal, whichever comes first.
async fn next_or_signal(
    queue: &cortex_core::EventQueue,
    interrupted: &mut mpsc::UnboundedReceiver<Interrupt>,
) -> Next {
    let mut outgoing = std::pin::pin!(queue.next());
    let mut signals_open = true;
    std::future::poll_fn(|context| {
        if signals_open {
            match interrupted.poll_recv(context) {
                std::task::Poll::Ready(Some(interrupt)) => {
                    return std::task::Poll::Ready(Next::Signal(interrupt));
                }
                std::task::Poll::Ready(None) => signals_open = false,
                std::task::Poll::Pending => {}
            }
        }
        outgoing
            .as_mut()
            .poll(context)
            .map(|next| next.map_or(Next::Closed, Next::Outgoing))
    })
    .await
}

/// Starts the core with what the command line says.
fn start_core(plan: &Plan) -> Result<Core, OpError> {
    let run = &plan.run;
    let mut options = CoreOptions::new(plan.env.clone(), Arc::new(TokioTimer));
    options.profile.clone_from(&run.common.profile);
    options.overrides = overrides(run);
    for (texts, action) in [
        (&run.allow, RuleAction::Allow),
        (&run.deny, RuleAction::Deny),
    ] {
        for text in texts {
            let rule = parse_rule(text, action).map_err(|message| {
                Box::new(ErrorData::new(ErrorKind::Usage, "invalid_rule", message))
            })?;
            options.rules.push(rule);
        }
    }
    options.trust_project = run.trust_project;
    options.fail_on_denial = run.fail_on_denial;
    options.system_append.clone_from(&run.system_append);
    options.guard.clone_from(&plan.guard);
    options.cancel = CancelToken::new();
    if let Some(log) = &plan.log {
        let log = log.clone();
        options.key_found = Some(Arc::new(move |secret: &cortex_base::Secret| {
            log.add_secret(secret);
        }));
    }
    Core::start(options)
}

/// The settings the command line gives, in order.
pub(crate) fn overrides(run: &Run) -> Vec<Override> {
    run.common
        .settings
        .iter()
        .map(|(key, value, flag)| Override {
            key: key.clone(),
            value: value.clone(),
            flag: flag.clone(),
        })
        .collect()
}

/// Opens the session the run is in.
async fn session(
    core: &Core,
    client: cortex_core::ClientId,
    plan: &Plan,
) -> Result<SessionState, OpError> {
    let workspace = plan.workspace.to_string_lossy().into_owned();
    let open = |session_id: String| {
        Op::SessionOpen(SessionRef {
            session_id: SessionId::from(session_id),
        })
    };
    let op = match &plan.run.choice {
        Choice::New => Op::SessionNew(SessionNewParams {
            workspace,
            settings: SessionSettings::default(),
            title: None,
            config: serde_json::Map::new(),
            ephemeral: plan.run.no_session,
        }),
        Choice::Resume(id) => open(id.clone()),
        Choice::Fork(id) => {
            // A session is forked from its log, so it is opened first.
            core.handle(client, open(id.clone())).await?;
            Op::SessionFork(SessionForkParams {
                session_id: SessionId::from(id.as_str()),
                at: None,
            })
        }
        Choice::Continue => {
            let listed = core
                .handle(
                    client,
                    Op::SessionList(SessionListParams {
                        workspace: Some(workspace),
                        limit: Some(1),
                        cursor: None,
                    }),
                )
                .await?;
            let latest = match listed {
                OpResult::SessionList(list) => list.sessions.into_iter().next(),
                _ => None,
            };
            let Some(latest) = latest else {
                return Err(Box::new(ErrorData::new(
                    ErrorKind::Usage,
                    "no_session",
                    "there is no session to continue in this workspace",
                )));
            };
            open(latest.session_id.as_str().to_owned())
        }
    };
    match core.handle(client, op).await? {
        OpResult::SessionNew(result)
        | OpResult::SessionOpen(result)
        | OpResult::SessionFork(result) => Ok(result.session),
        _ => Err(Box::new(ErrorData::new(
            ErrorKind::Internal,
            "unexpected_result",
            "the core answered with the result of another operation",
        ))),
    }
}

/// The prompt: standard input when it is not a terminal, then the words
/// given, with a blank line between. `None` when there is neither.
fn prompt(words: Option<&str>) -> io::Result<Option<String>> {
    let stdin = io::stdin();
    let piped = if stdin.is_terminal() {
        String::new()
    } else {
        // Bytes that are not UTF-8 (a diff of a file in another encoding)
        // are read as U+FFFD rather than failing the run.
        let mut bytes = Vec::new();
        stdin.lock().take(STDIN_LIMIT).read_to_end(&mut bytes)?;
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let piped = piped.trim_end();
    let words = words.map(str::trim).unwrap_or_default();
    Ok(match (piped.is_empty(), words.is_empty()) {
        (true, true) => None,
        (true, false) => Some(words.to_owned()),
        (false, true) => Some(piped.to_owned()),
        (false, false) => Some(format!("{piped}\n\n{words}")),
    })
}

/// Starts the log: the file in the state directory's `logs/`, and the
/// stderr sink at `warn` (`info` with `--verbose`, off with `--quiet`).
/// A log that cannot start is said once, and the run goes on without it.
fn start_log(run: &Run, env: &Env, name: &str) -> Option<Arc<LogHandle>> {
    let clock = Arc::new(SystemClock::new());
    let mut options = LogOptions::new(clock);
    let level = run
        .log_level
        .clone()
        .or_else(|| env.get("CORTEX_LOG").map(str::to_owned));
    if let Some(level) = level {
        match Filter::parse(&level) {
            Ok(filter) => options.filter = filter,
            Err(error) if !run.quiet => {
                eprintln!("{name}: the log level `{level}` is not understood: {error}");
            }
            Err(_) => {}
        }
    }
    options.file = StateDir::from_dirs(&Dirs::from_env(env))
        .map(|state| FileOptions::in_dir(state.logs_dir()));
    if !run.quiet {
        let level = if run.verbose {
            LevelFilter::INFO
        } else {
            LevelFilter::WARN
        };
        options.stderr = Some((level, Box::new(io::stderr())));
    }
    options.secrets = Arc::new(SecretFilter::new());
    let (subscriber, handle) = match log::start(options) {
        Ok(started) => started,
        Err(error) => {
            if !run.quiet {
                eprintln!("{name}: the log file could not be opened: {error}");
            }
            return None;
        }
    };
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        return None;
    }
    Some(Arc::new(handle))
}

/// Writes each line to standard output and flushes it. Stops writing (and
/// keeps taking lines, so the core never waits on it) once standard output
/// is closed.
fn write_lines(mut lines: mpsc::Receiver<String>) {
    let stdout = io::stdout();
    let mut open = true;
    while let Some(line) = lines.blocking_recv() {
        if !open {
            continue;
        }
        let mut out = stdout.lock();
        if out
            .write_all(line.as_bytes())
            .and_then(|()| out.flush())
            .is_err()
        {
            open = false;
        }
    }
}

fn interrupted_error() -> ErrorData {
    ErrorData::new(
        ErrorKind::Interrupted,
        "interrupted",
        "the run was interrupted before its prompt was sent",
    )
}

fn elapsed(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
