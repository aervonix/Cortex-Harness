//! Integration tests (tier 2): the core, end to end and in process.
//!
//! Every test drives a real [`Core`] as a front end does: it connects a
//! client, sends operations and reads the client's queue. Behind the core
//! is everything real but the model, which is the scripted provider of the
//! test kit: the real loop, the real tools on a real file system, the real
//! session log, the real permission engine and the real configuration
//! loader. Nothing waits: time is the fake clock, and a wait ends when a
//! test ends it.
//!
//! A test's files are under the system's temporary directory, outside this
//! repository, so that a test which needs a workspace with no repository
//! above it has one.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    missing_debug_implementations,
    unreachable_pub,
    reason = "an integration test prepares real files and fails by panicking, and its \
              helpers are shared between the modules of this one binary"
)]

mod approvals;
mod context;
mod events;
mod gates;
mod harness_git;
mod lifecycle;
mod plan;
mod provider;
mod sandbox;
mod secrets;
mod settings;
mod shell;
mod start;
mod trust;
mod turns;

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cortex_base::CancelToken;
use cortex_config::{Dirs, Env, Platform};
use cortex_core::{Client, ClientInfo, Connector, Core, CoreOptions, OpError, Outgoing};
use cortex_llm::{ImageSource, ModelClient, ProviderInfo};
use cortex_protocol::event::{Event, Notice, TurnCompleted};
use cortex_protocol::jsonrpc::RequestId;
use cortex_protocol::methods::{
    ApprovalChoice, ApprovalRequest, ApprovalResponse, Op, OpResult, ServerRequest, ServerResponse,
    SessionNewParams, SessionRef, TurnInterruptParams, TurnStartParams,
};
use cortex_protocol::session::{SessionSettings, SessionState};
use cortex_protocol::{Item, Part, SessionEvent, SessionId, StopReason};
use cortex_testkit::{FakeClock, FakeTimer, Response, ScriptedProvider};
use cortex_tools::process::Runner;
use serde_json::{Value, json};

/// The model the tests' configuration names: one of the built-in catalog,
/// with a fast tier.
pub const MODEL: &str = "claude-opus-5-5";
/// Another model of the built-in catalog.
pub const OTHER_MODEL: &str = "claude-haiku-5-5";

/// The argument that makes this binary the sandbox launcher, as the
/// harness's hidden subcommand makes the real one.
pub const LAUNCH: &str = "cx-launch";

/// The same for the harness's own git. This binary writes a line of its
/// own before it becomes the launcher (the test runner's "running 1 test"),
/// which the real binary does not: a command's output can carry it, and
/// git's answer, which the harness reads, cannot.
pub const LAUNCH_GIT: &str = "cx-launch-git";

/// What the test runner runs as an ordinary test, which passes at once, and
/// what the sandbox tests start as the launcher.
#[test]
fn launcher_entry() {
    let arguments: Vec<OsString> = std::env::args_os().collect();
    if let Some(at) = arguments.iter().position(|argument| argument == LAUNCH_GIT) {
        // The harness's own git, which `sandbox::launcher`'s wrapper
        // started with its output on standard error and the test runner's
        // first line on nowhere: a shell inside the sandbox gives git its
        // output back, and its standard error to nowhere, as the harness
        // does.
        let mut rest = arguments[at + 1..].to_vec();
        rest.splice(
            1..1,
            ["/bin/sh", "-c", "exec \"$0\" \"$@\" 1>&2 2>/dev/null"].map(OsString::from),
        );
        let code = cortex_sandbox::launch::run(rest);
        #[expect(
            clippy::exit,
            reason = "the launcher ends as the hidden subcommand does"
        )]
        std::process::exit(code);
    }
    if let Some(at) = arguments.iter().position(|argument| argument == LAUNCH) {
        let code = cortex_sandbox::launch::run(arguments[at + 1..].to_vec());
        #[expect(
            clippy::exit,
            reason = "the launcher ends as the hidden subcommand does"
        )]
        std::process::exit(code);
    }
}

/// Gives every provider the scripted client.
struct Scripted(ScriptedProvider);

impl Connector for Scripted {
    fn connect(
        &self,
        _provider: &ProviderInfo,
        _images: Arc<dyn ImageSource>,
    ) -> Option<Arc<dyn ModelClient>> {
        Some(Arc::new(self.0.clone()))
    }
}

/// A machine of the test's own: a home directory, a workspace, a clock, a
/// timer and a scripted model.
pub struct World {
    pub dir: PathBuf,
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub runtime: tokio::runtime::Runtime,
    pub clock: FakeClock,
    pub timer: FakeTimer,
    pub provider: ScriptedProvider,
    pub env: Env,
    /// The loop's blocking work (planning a call, bounding a result) and
    /// the core's own (putting a session together, a rewind's work on the
    /// log) run on the test's thread, in the order this run's seed gives.
    /// The session store's writes, and a command, stay real threads and
    /// processes: their order is the machine's.
    _order: Option<cortex_tools::InlineBlocking>,
    /// Removes the test's directory when the test ends, unless it failed.
    _scratch: cortex_testkit::Scratch,
}

impl World {
    pub fn new(test: &str, responses: impl IntoIterator<Item = Response>) -> Self {
        // In the system's temporary directory, so that no repository is
        // above the workspace.
        let scratch = cortex_testkit::Scratch::outside("cortex-core-it", test);
        let dir = scratch.path().to_path_buf();
        // Printed by the test kit, which the test runner shows if the test
        // fails.
        let seed = cortex_testkit::seed::for_this_test();
        let home = dir.join("home");
        let workspace = dir.join("work").join("app");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let home_variable = match Platform::CURRENT {
            Platform::Unix => "HOME",
            Platform::Windows => "USERPROFILE",
        };
        let env = Env::empty(Platform::CURRENT).with(home_variable, home.to_str().unwrap());
        let clock = FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap();
        let timer = FakeTimer::manual(&clock);
        let world = Self {
            runtime: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
            provider: ScriptedProvider::new(responses),
            _order: Some(cortex_tools::InlineBlocking::seeded(seed)),
            _scratch: scratch,
            dir,
            home,
            workspace,
            clock,
            timer,
            env,
        };
        world.user_config("");
        world
    }

    /// Makes every piece of blocking work, the loop's and the core's, wait
    /// exactly one turn of the runtime, in place of the seed's order: for a
    /// test that polls an operation by hand and acts between two of its
    /// steps. Each piece is then one `Pending` of the operation that waits
    /// for it.
    pub fn one_turn_each(&mut self) {
        // The seeded order goes first: dropping it puts back the order
        // that was before it, which is none.
        self._order = None;
        self._order = Some(cortex_tools::InlineBlocking::after(1));
    }

    /// Where this machine's configuration, state and data are. Nothing of
    /// the real machine's is read.
    pub fn dirs(&self) -> Dirs {
        let mut dirs = Dirs::from_env(&self.env);
        dirs.managed_config = Some(self.dir.join("managed").join("config.toml"));
        dirs
    }

    /// Writes the user's configuration: the main model, and `extra`.
    pub fn user_config(&self, extra: &str) {
        let path = self.dirs().user_config.unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Nothing is flushed to the disk: a flush takes as long as the
        // disk takes, and is not what these tests check.
        std::fs::write(
            path,
            // The credentials are a file of the test's own on every OS:
            // the machine's keychain is never read.
            format!(
                "[session]\ndurability = \"never\"\n\n[credentials]\nstore = \
                 \"file\"\n\n[model]\nmain = \"{MODEL}\"\n\n{extra}"
            ),
        )
        .unwrap();
    }

    /// Writes the user's configuration as it is given, with nothing added.
    pub fn raw_user_config(&self, content: &str) {
        let path = self.dirs().user_config.unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Writes the project's configuration.
    pub fn project_config(&self, content: &str) {
        let dir = self.workspace.join(".cortex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), content).unwrap();
    }

    /// Puts a file in the workspace.
    pub fn put(&self, relative: &str, content: impl AsRef<[u8]>) {
        let path = self.workspace.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A file of the workspace, as text.
    pub fn read(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace.join(relative)).ok()
    }

    /// The options of a core on this machine: the scripted model, no base
    /// state taken, and a process runner whose waits end only when a test
    /// ends them.
    pub fn options(&self) -> CoreOptions {
        let timer: Arc<FakeTimer> = Arc::new(self.timer.clone());
        let mut options = CoreOptions::new(self.env.clone(), timer.clone());
        options.dirs = Some(self.dirs());
        options.clock = Arc::new(self.clock.clone());
        options.harness_version = "0.0.0-test".to_owned();
        options.connector = Some(Arc::new(Scripted(self.provider.clone())));
        options.base_capture = false;
        options.runner = Some(
            Runner::new()
                .with_timer(timer)
                .with_clock(Arc::new(self.clock.clone()))
                .with_frame(Duration::ZERO),
        );
        // The machine's shell, found once for the whole test binary and
        // not asked its version: no process is started to open a session.
        static SHELL: std::sync::OnceLock<Option<cortex_tools::Shell>> = std::sync::OnceLock::new();
        options.shell = SHELL
            .get_or_init(|| cortex_tools::ShellSearch::from_env().find(None, None).ok())
            .clone();
        // A command inherits a small environment of the test's own, never
        // the developer's.
        options.shell_env = Some(self.shell_env(&[]));
        options.cancel = CancelToken::new();
        options
    }

    /// An environment for shell commands: what is needed to find programs,
    /// and `extra`.
    pub fn shell_env(&self, extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        // On Windows a shell needs more of the machine's environment than
        // can be listed here (PowerShell reads a dozen variables to start),
        // so there the test's own environment is the base.
        let mut env: Vec<(OsString, OsString)> = if cfg!(windows) {
            std::env::vars_os().collect()
        } else {
            ["PATH", "TMPDIR"]
                .iter()
                .filter_map(|name| Some((OsString::from(name), std::env::var_os(name)?)))
                .collect()
        };
        for (name, value) in extra {
            env.retain(|(known, _)| !known.eq_ignore_ascii_case(name));
            env.push((OsString::from(name), OsString::from(value)));
        }
        env
    }

    pub fn core(&self) -> Core {
        self.core_with(|_| {})
    }

    pub fn core_with(&self, change: impl FnOnce(&mut CoreOptions)) -> Core {
        let mut options = self.options();
        change(&mut options);
        Core::start(options).expect("the core starts")
    }

    pub fn run<T>(&self, future: impl Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }

    /// Where the sessions of this machine are kept.
    pub fn sessions_dir(&self) -> PathBuf {
        self.dirs().data.unwrap().join("sessions")
    }

    /// The one session log on this machine, as text.
    pub fn log(&self) -> String {
        let logs = self.logs();
        assert_eq!(logs.len(), 1, "one session log: {logs:?}");
        std::fs::read_to_string(&logs[0]).unwrap()
    }

    /// Every session log on this machine.
    pub fn logs(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.file_name().is_some_and(|name| name == "log.jsonl") {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.sessions_dir(), &mut found);
        found.sort();
        found
    }

    /// Every file under the data directory, with its content: what a
    /// secret must not be in.
    pub fn stored(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, found: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else {
                    let content = std::fs::read(&path).unwrap_or_default();
                    found.push((path, content));
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.dirs().data.unwrap(), &mut found);
        found
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A front end: a client of a core, with a session open.
pub struct Front {
    pub core: Core,
    pub client: Client,
    pub session: SessionId,
    pub state: SessionState,
    /// Everything the client has been sent, in order.
    pub seen: std::cell::RefCell<Vec<Outgoing>>,
    /// The approval requests it has taken up.
    pub answered: std::cell::RefCell<Vec<RequestId>>,
}

impl Front {
    /// Connects a user's front end and starts a session in the world's
    /// workspace.
    pub async fn start(world: &World, core: &Core) -> Self {
        Self::start_as(world, core, ClientInfo::user(), SessionSettings::default())
            .await
            .expect("the session starts")
    }

    pub async fn start_as(
        world: &World,
        core: &Core,
        info: ClientInfo,
        settings: SessionSettings,
    ) -> Result<Self, OpError> {
        let client = core.connect(info);
        let result = core
            .handle(
                client.id,
                Op::SessionNew(SessionNewParams {
                    workspace: world.workspace.to_str().unwrap().to_owned(),
                    settings,
                    title: None,
                    config: serde_json::Map::new(),
                    ephemeral: false,
                }),
            )
            .await?;
        let OpResult::SessionNew(result) = result else {
            panic!("not the result of session/new");
        };
        Ok(Self {
            core: core.clone(),
            client,
            session: result.session.session_id.clone(),
            state: result.session,
            seen: std::cell::RefCell::new(Vec::new()),
            answered: std::cell::RefCell::new(Vec::new()),
        })
    }

    /// Another client on the same session.
    pub async fn join(&self, info: ClientInfo) -> Self {
        let client = self.core.connect(info);
        let result = self
            .core
            .handle(
                client.id,
                Op::SessionOpen(SessionRef {
                    session_id: self.session.clone(),
                }),
            )
            .await
            .expect("the session opens");
        let OpResult::SessionOpen(result) = result else {
            panic!("not the result of session/open");
        };
        Self {
            core: self.core.clone(),
            client,
            session: self.session.clone(),
            state: result.session,
            seen: std::cell::RefCell::new(Vec::new()),
            answered: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub async fn op(&self, op: Op) -> Result<OpResult, OpError> {
        self.core.handle(self.client.id, op).await
    }

    pub async fn prompt(&self, text: &str) {
        self.op(Op::TurnStart(TurnStartParams {
            session_id: self.session.clone(),
            input: vec![Part::text(text)],
            delivery: None,
        }))
        .await
        .expect("the prompt is accepted");
    }

    pub async fn interrupt(&self, now: bool) {
        self.op(Op::TurnInterrupt(TurnInterruptParams {
            session_id: self.session.clone(),
            now,
        }))
        .await
        .expect("the interrupt is accepted");
    }

    pub async fn close(&self) {
        self.op(Op::SessionClose(SessionRef {
            session_id: self.session.clone(),
        }))
        .await
        .expect("the session closes");
    }

    /// Reads the client's queue until `done` holds of everything seen so
    /// far.
    pub async fn until(&self, done: impl Fn(&[Outgoing]) -> bool) {
        loop {
            if done(&self.seen.borrow()) {
                return;
            }
            let next = self
                .client
                .queue
                .next()
                .await
                .expect("the client is connected");
            self.seen.borrow_mut().push(next);
        }
    }

    /// Reads what is in the queue now, without waiting.
    pub fn drain(&self) {
        while let Some(next) = self.client.queue.try_next() {
            self.seen.borrow_mut().push(next);
        }
    }

    /// Waits until `count` turns have completed.
    pub async fn turns(&self, count: usize) {
        self.until(|seen| ends_in(seen).len() >= count).await;
    }

    /// Sends a prompt and waits for its turn to end.
    pub async fn turn(&self, text: &str) -> TurnCompleted {
        let before = self.ends().len();
        self.prompt(text).await;
        self.turns(before + 1).await;
        self.ends().pop().unwrap()
    }

    /// Sends a prompt and waits for its turn to end, allowing every call
    /// that is asked about, once. Returns the tools that were asked about,
    /// in order.
    pub async fn turn_allowing(&self, text: &str) -> Vec<String> {
        let before = self.ends().len();
        self.prompt(text).await;
        let mut asked = Vec::new();
        loop {
            self.until(|seen| {
                ends_in(seen).len() > before
                    || requests_in(seen)
                        .iter()
                        .any(|(id, _)| !self.answered.borrow().contains(id))
            })
            .await;
            if self.ends().len() > before {
                return asked;
            }
            let (id, request) = self.approval().await;
            asked.push(request.tool.clone());
            self.answer(&id, ApprovalChoice::AllowOnce).unwrap();
        }
    }

    /// The session's log on `world`'s disk, a line an entry.
    pub fn log(&self, world: &World) -> Vec<Value> {
        let path = world
            .logs()
            .into_iter()
            .find(|path| path.to_string_lossy().contains(self.session.as_str()))
            .expect("the session has a log");
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Waits for the next approval request that has not been answered.
    pub async fn approval(&self) -> (RequestId, ApprovalRequest) {
        let answered = self.answered.borrow().clone();
        self.until(|seen| {
            requests_in(seen)
                .iter()
                .any(|(id, _)| !answered.contains(id))
        })
        .await;
        let found = requests_in(&self.seen.borrow())
            .into_iter()
            .find(|(id, _)| !answered.contains(id))
            .unwrap();
        self.answered.borrow_mut().push(found.0.clone());
        found
    }

    pub fn answer(&self, id: &RequestId, choice: ApprovalChoice) -> Result<(), OpError> {
        self.core.respond(
            self.client.id,
            id,
            ServerResponse::Approval(ApprovalResponse { choice, note: None }),
        )
    }

    /// Every approval this client was asked for, in order.
    pub fn approvals(&self) -> Vec<ApprovalRequest> {
        requests_in(&self.seen.borrow())
            .into_iter()
            .map(|(_, request)| request)
            .collect()
    }

    pub fn events(&self) -> Vec<SessionEvent> {
        self.seen
            .borrow()
            .iter()
            .filter_map(|outgoing| match outgoing {
                Outgoing::Event(event) => Some(event.clone()),
                Outgoing::Request { .. } => None,
            })
            .collect()
    }

    pub fn ends(&self) -> Vec<TurnCompleted> {
        ends_in(&self.seen.borrow())
    }

    /// The items that completed, in order.
    pub fn items(&self) -> Vec<Item> {
        self.events()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::ItemCompleted(completed) => Some(completed.item),
                _ => None,
            })
            .collect()
    }

    /// The text of every `info` and `sandbox` notice, in order.
    pub fn notices(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::Notice(Notice::Info(info)) => Some(info.message),
                Event::Notice(Notice::Sandbox(sandbox)) => Some(sandbox.message),
                _ => None,
            })
            .collect()
    }

    /// The text of the last assistant message.
    pub fn last_text(&self) -> String {
        self.items()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::AssistantMessage(message) => Some(
                    message
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The tool results, in order: the tool, how the call ended, and what
    /// the model was shown.
    pub fn results(&self) -> Vec<(String, String, String)> {
        self.items()
            .into_iter()
            .filter_map(|item| match item {
                Item::ToolResult(result) => Some((
                    result.tool.clone(),
                    result.outcome.as_str().to_owned(),
                    result
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                )),
                _ => None,
            })
            .collect()
    }
}

fn ends_in(seen: &[Outgoing]) -> Vec<TurnCompleted> {
    seen.iter()
        .filter_map(|outgoing| match outgoing {
            Outgoing::Event(SessionEvent {
                event: Event::TurnCompleted(completed),
                ..
            }) => Some(completed.clone()),
            _ => None,
        })
        .collect()
}

fn requests_in(seen: &[Outgoing]) -> Vec<(RequestId, ApprovalRequest)> {
    seen.iter()
        .filter_map(|outgoing| match outgoing {
            Outgoing::Request {
                id,
                request: ServerRequest::Approval(request),
            } => Some((id.clone(), request.clone())),
            _ => None,
        })
        .collect()
}

/// A response that calls tools.
pub fn calls(calls: &[(&str, &str, Value)]) -> Response {
    let mut response = Response::new();
    for (id, tool, arguments) in calls {
        response = response.tool_call(id, tool, arguments);
    }
    response.end(StopReason::ToolCalls)
}

/// A response that says something and ends the turn.
/// The text of a message's text parts, joined.
pub fn texts(message: &cortex_protocol::Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

pub fn says(text: &str) -> Response {
    Response::new().text(text).end(StopReason::End)
}

/// A shell command for the machine's own shell that prints `text`.
pub fn echo(text: &str) -> Value {
    json!({ "command": format!("echo {text}") })
}

/// Polls `future` once, by hand: what it returns now, if it is done.
pub async fn poll_once<F: Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
    let mut future = future;
    std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await
}

/// Lets every task that can run without the test's help run.
pub async fn settle() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}
