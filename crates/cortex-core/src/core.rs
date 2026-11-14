//! The core: operations in, events out
//! (`docs/architecture/00-overview.md`, `docs/architecture/11-rpc-protocol.md`).
//!
//! A [`Core`] holds the sessions of one process. A front end connects
//! ([`Core::connect`]), sends operations ([`Core::handle`]) and reads what
//! comes back from its queue: the events of the sessions it has open, and
//! requests it is to answer ([`Core::respond`]). The terminal UI, headless
//! mode and the RPC server are all front ends in this sense, and none of
//! them reaches a session any other way.
//!
//! **Where it runs.** On a Tokio runtime, which the caller provides: the
//! core thread's current-thread runtime (`docs/architecture/02-runtime.md`).
//! A session's loop is one task on it, and a shell command is started from
//! that thread. Every method of [`Core`] may be called from any thread;
//! the ones that start or resume a session must be called inside the
//! runtime.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the blocking calls here are inside closures run on the blocking pool"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cortex_agent::{Session, SessionHandle};
use cortex_base::{
    CancelToken, IdGenerator, Secret, SharedClock, SharedTimer, SystemClock, SystemRng,
};
use cortex_config::{Env, LoadOptions, Loaded, Override, StateDir};
use cortex_llm::{ImageSource, Protocol, SpeedCooldowns, Transport};
use cortex_protocol::error::code;
use cortex_protocol::event::{Actor, Event, PlanUpdated};
use cortex_protocol::item::NoticeSource;
use cortex_protocol::jsonrpc::RequestId;
use cortex_protocol::lease::{Level, Rule};
use cortex_protocol::methods::{
    ApprovalScope, ClientRole, ConfigGetResult, Empty, ExtensionsStatusResult, InitializeResult,
    McpStatusResult, ModelsListResult, Op, OpResult, PROTOCOL_VERSION, PeerInfo,
    PermissionsListResult, PlanResult, SUPPORTED_PROTOCOL_VERSIONS, ServerCapabilities,
    ServerResponse, SessionListResult, SessionReadResult, SessionResult, UsageGetResult,
    negotiate_version,
};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::{SandboxStatus, SessionSettings, SessionSummary};
use cortex_protocol::{ErrorData, ErrorKind, Item, ItemId, LeaseId, SessionId};
use cortex_session::entry::{LeaseEvent, LeaseRecord};
use cortex_session::{Body, Git, Restore, SessionsRoot, SystemGit};
use cortex_tools::PlanBoard;
use cortex_tools::process::Runner;
use tokio::task::JoinHandle;

use crate::assemble::{self, Assembled, EngineTemplate, Wanted, Which};
use crate::clients::{ClientId, ClientInfo, Clients};
use crate::events::{EventQueue, SessionSink};
use crate::policy::{EnginePolicy, PermissionDenial};
use crate::provider::{self, Connector, Providers};
use crate::store::{Hooks, SharedLog};

/// What a change of model, reasoning level or speed costs once, said to
/// the user when it is made in a session that has history.
const UNCACHED: &str = "The next request sends the whole conversation again without the \
                        provider's cached prefix: it costs more and takes longer, once.";

/// How long the end of a session waits for the capture of the workspace's
/// starting state, when that has not finished.
const BASE_WAIT: Duration = Duration::from_secs(2);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each change behind these locks is whole before the lock is let go.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn error(kind: ErrorKind, code: &str, message: impl Into<String>) -> Box<ErrorData> {
    Box::new(ErrorData::new(kind, code, message))
}

/// What an operation ends with when it does not succeed.
pub type OpError = Box<ErrorData>;

/// What a core is started with.
pub struct CoreOptions {
    /// The environment: where configuration and credentials are looked
    /// for. The binary builds it from the process's own.
    pub env: Env,
    /// Where configuration, state and data are, in place of where `env`
    /// says: a test's, which must not read the machine's own files.
    pub dirs: Option<cortex_config::Dirs>,
    /// The time of everything.
    pub clock: SharedClock,
    /// Every wait: retries, time limits, the stages of stopping a command,
    /// an approval's time limit.
    pub timer: SharedTimer,
    /// The version of Cortex-Harness, recorded in every session it creates.
    pub harness_version: String,
    /// The profile to load configuration with (`--profile`).
    pub profile: Option<String>,
    /// Text added to the end of every session's system prompt
    /// (`--system-append`). The user's own, for this run.
    pub system_append: Option<String>,
    /// Settings from the command line (`-c key=value`, and flags such as
    /// `--model`, `--effort`, `--speed` and `--permission-mode` as their
    /// keys). They rank above every file and are the user's own, so
    /// nothing in them waits for trust.
    pub overrides: Vec<Override>,
    /// Permission rules from the command line (`--allow`, `--deny`).
    pub rules: Vec<Rule>,
    /// Treat every project as trusted for this run (`--trust-project`).
    pub trust_project: bool,
    /// End the turn at the first call that needs an answer nobody can give
    /// (`--fail-on-denial`). Without it such a call is refused and the run
    /// goes on. Either way the refusal is kept
    /// ([`Core::permission_denials`]).
    pub fail_on_denial: bool,
    /// How long an approval request waits for its answer before it is a
    /// denial. `None` waits as long as it takes, which is what a person at
    /// a terminal wants.
    pub approval_timeout: Option<Duration>,
    /// The guard of process trees: the program to start (the harness's own
    /// executable) and its arguments (the hidden subcommand whose body is
    /// `cortex_sandbox::tree::run_guard`). `None`: commands run without a
    /// guard, and can outlive a harness that is killed.
    pub guard: Option<(PathBuf, Vec<OsString>)>,
    /// The sandbox launcher: the program to start (the harness's own
    /// executable) and its arguments (the hidden subcommand whose body is
    /// `cortex_sandbox::launch::run`). Every shell command is started
    /// through it where the sandbox enforces. `None`: nothing is confined,
    /// and every session reports the sandbox off.
    pub launcher: Option<(PathBuf, Vec<OsString>)>,
    /// What the system offers the sandbox, in place of asking it
    /// (`cortex_sandbox::support::detect`): a test's, which says the kernel
    /// has no Landlock, or an old one.
    pub sandbox: Option<cortex_sandbox::support::Support>,
    /// How far the look at protected paths around a confined command goes,
    /// in place of its standard bounds: a test's, which reaches them.
    pub protect_limits: Option<cortex_sandbox::protect::Limits>,
    /// API keys given on the command line, by provider. Looked at before
    /// the environment and the credential store.
    pub api_keys: BTreeMap<String, Secret>,
    /// Told every provider key the core finds, once each: the place to
    /// give it to the log's own filter
    /// (`cortex_base::log::LogHandle::add_secret`), which was started
    /// before any key was known.
    pub key_found: Option<provider::KeyFound>,
    /// Makes model clients in place of the built-in way: a test's scripted
    /// provider.
    pub connector: Option<Arc<dyn Connector>>,
    /// What requests are sent through. `None`: the HTTP transport, which
    /// costs nothing until the first request.
    pub transport: Option<Arc<dyn Transport>>,
    /// Protocols beside the built-in ones, by name: a test's.
    pub protocols: BTreeMap<String, Arc<dyn Protocol>>,
    /// The environment shell commands inherit from, in place of the
    /// process's own: a test's.
    pub shell_env: Option<Vec<(OsString, OsString)>>,
    /// The process runner shell commands go through, in place of one on
    /// this core's timer and clock with the guard above: a test's.
    pub runner: Option<Runner>,
    /// The shell commands run in, in place of the one that is looked for
    /// and asked its version: a test's, which starts no process to find
    /// out what the model's tool description says.
    pub shell: Option<cortex_tools::Shell>,
    /// How git is run for the base state and for a file's earlier content.
    pub git: Arc<dyn Git>,
    /// Whether a new session records the workspace's starting state.
    pub base_capture: bool,
    /// Cancelled when the process is to stop: every session's token is a
    /// child of it.
    pub cancel: CancelToken,
}

impl std::fmt::Debug for CoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreOptions")
            .field("env", &self.env)
            .field("harness_version", &self.harness_version)
            .field("profile", &self.profile)
            .field("trust_project", &self.trust_project)
            .field("fail_on_denial", &self.fail_on_denial)
            .field("approval_timeout", &self.approval_timeout)
            .finish_non_exhaustive()
    }
}

impl CoreOptions {
    /// The options of a core in `env`, on `timer`, with the system's clock
    /// and nothing from a command line.
    #[must_use]
    pub fn new(env: Env, timer: SharedTimer) -> Self {
        Self {
            env,
            dirs: None,
            clock: Arc::new(SystemClock::new()),
            timer,
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            profile: None,
            system_append: None,
            overrides: Vec::new(),
            rules: Vec::new(),
            trust_project: false,
            fail_on_denial: false,
            approval_timeout: None,
            guard: None,
            launcher: None,
            sandbox: None,
            protect_limits: None,
            api_keys: BTreeMap::new(),
            key_found: None,
            connector: None,
            transport: None,
            protocols: BTreeMap::new(),
            shell_env: None,
            runner: None,
            shell: None,
            git: Arc::new(SystemGit),
            base_capture: true,
            cancel: CancelToken::new(),
        }
    }
}

/// A session that is open.
pub(crate) struct Live {
    pub(crate) handle: SessionHandle,
    task: Mutex<Option<JoinHandle<()>>>,
    pub(crate) sink: Arc<SessionSink>,
    pub(crate) log: SharedLog,
    pub(crate) hooks: Arc<Hooks>,
    pub(crate) policy: Arc<EnginePolicy>,
    pub(crate) board: Arc<PlanBoard>,
    /// The session's sandbox: its launcher and its temporary directory.
    sandbox: Arc<crate::sandbox::SessionSandbox>,
    template: Mutex<EngineTemplate>,
    loaded: Arc<Loaded>,
    images: Arc<dyn ImageSource>,
    lease_count: AtomicU64,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("session", self.handle.id())
            .finish_non_exhaustive()
    }
}

/// What a core is, behind its handle.
pub(crate) struct Inner {
    pub(crate) options: CoreOptions,
    pub(crate) clients: Arc<Clients>,
    pub(crate) providers: Providers,
    /// The configuration with no project's layers: what `config/get`
    /// answers with when it is asked about no session.
    pub(crate) user: Arc<Loaded>,
    ids: IdGenerator,
    sessions: Mutex<BTreeMap<SessionId, Arc<Live>>>,
    /// The shells found so far, by the program and arguments configured:
    /// a shell is asked its version once in the life of a core.
    pub(crate) shells: Mutex<BTreeMap<ShellKey, cortex_tools::Shell>>,
    /// The threads asking a shell its version, to keep the answer for the
    /// runs after this one ([`crate::shell_cache`]). [`Core::shutdown`]
    /// waits for them, so that a short run still keeps it.
    pub(crate) probes: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Set when [`Core::shutdown`] begins: no session starts after it.
    shutting_down: AtomicBool,
    /// What the system offers the sandbox, asked once.
    pub(crate) sandbox: crate::sandbox::CoreSandbox,
}

/// What a shell was looked for with: `shell.program` and `shell.args`.
pub(crate) type ShellKey = (Option<String>, Option<Vec<String>>);

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("sessions", &lock(&self.sessions).len())
            .finish_non_exhaustive()
    }
}

/// A front end's connection: its number, and the queue everything for it
/// arrives in.
#[derive(Debug, Clone)]
pub struct Client {
    /// The client's number, which every operation names.
    pub id: ClientId,
    /// What the core sends it: the events of the sessions it has open, and
    /// requests to answer.
    pub queue: Arc<EventQueue>,
}

/// The core. Clones are the same core.
#[derive(Debug, Clone)]
pub struct Core {
    pub(crate) inner: Arc<Inner>,
}

impl Core {
    /// Starts a core: reads the user's configuration, the model catalog
    /// with the user's entries, and opens the credential store. No session
    /// is started and no network connection is made.
    ///
    /// # Errors
    ///
    /// When the user's configuration is invalid, or an entry of the user's
    /// model catalog cannot be read: the error names the file, the entry
    /// and what is wrong with it, and nothing is started.
    pub fn start(options: CoreOptions) -> Result<Self, OpError> {
        let mut load = LoadOptions::new(options.env.clone());
        if let Some(dirs) = &options.dirs {
            load.dirs = dirs.clone();
        }
        load.profile.clone_from(&options.profile);
        load.overrides.clone_from(&options.overrides);
        let user = cortex_config::load(&load).map_err(|failure| {
            error(
                ErrorKind::Config,
                "invalid_configuration",
                failure.render(&options.env),
            )
        })?;
        let catalog = provider::catalog(&user, &options.env)?;
        let store = cortex_config::open_store(
            user.settings.credentials.store,
            &user.dirs,
            Arc::clone(&options.clock),
        )
        .unwrap_or_else(|failure| {
            tracing::warn!(error = %failure, "the credential store could not be opened");
            Box::new(provider::Unopened(failure.to_string()))
        });
        let mut protocols = provider::builtin_protocols();
        protocols.extend(options.protocols.clone());
        let transport: Arc<dyn Transport> = match &options.transport {
            Some(transport) => Arc::clone(transport),
            None => Arc::new(cortex_llm::HttpTransport::default()),
        };
        let providers = Providers {
            catalog,
            cooldowns: SpeedCooldowns::new(Arc::clone(&options.clock)),
            transport,
            clock: Arc::clone(&options.clock),
            timer: Arc::clone(&options.timer),
            protocols,
            connector: options.connector.clone(),
            store,
            flags: options.api_keys.clone(),
            filter: Arc::new(std::sync::RwLock::new(cortex_base::SecretFilter::new())),
            key_found: options.key_found.clone(),
        };
        let ids = IdGenerator::new(Arc::clone(&options.clock), Box::new(SystemRng::new()));
        let sandbox = crate::sandbox::CoreSandbox::new(
            options
                .sandbox
                .unwrap_or_else(cortex_sandbox::support::detect),
            options.launcher.clone(),
        );
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                clients: Arc::new(Clients::default()),
                providers,
                user: Arc::new(user),
                ids,
                sessions: Mutex::new(BTreeMap::new()),
                shells: Mutex::new(BTreeMap::new()),
                probes: Mutex::new(Vec::new()),
                shutting_down: AtomicBool::new(false),
                sandbox,
            }),
        })
    }

    /// Connects a front end.
    #[must_use]
    pub fn connect(&self, info: ClientInfo) -> Client {
        let (id, queue) = self.inner.clients.connect(info);
        Client { id, queue }
    }

    /// Disconnects a front end: it has no session open any more, and its
    /// queue takes nothing more. Sessions it alone had open are closed.
    ///
    /// The client is taken out first, and the sessions looked at after: a
    /// session that is put together for it meanwhile either is in the table
    /// by then, and is closed here, or finds the client gone when it is
    /// put in the table, and closes itself.
    pub async fn disconnect(&self, client: ClientId) {
        self.inner.clients.disconnect(client);
        let sessions: Vec<Arc<Live>> = lock(&self.inner.sessions).values().cloned().collect();
        for live in sessions {
            if live.sink.unsubscribe(client.0) && live.sink.subscribers().is_empty() {
                self.end(live.handle.id()).await;
            }
        }
    }

    /// The sessions this core has open, by id, in id order: what a server's
    /// status shows, and what is ended at [`shutdown`](Self::shutdown).
    #[must_use]
    pub fn open_sessions(&self) -> Vec<SessionId> {
        lock(&self.inner.sessions).keys().cloned().collect()
    }

    /// The calls of a session that were refused because nobody could be
    /// asked about them: a headless result's `permission_denials`.
    #[must_use]
    pub fn permission_denials(&self, session: &SessionId) -> Vec<PermissionDenial> {
        self.live(session)
            .map(|live| live.policy.denials())
            .unwrap_or_default()
    }

    /// Takes a client's answer to a request the core put to it.
    ///
    /// # Errors
    ///
    /// `role_not_allowed` when the client is not a `user` client or was not
    /// asked; an error of kind `usage` when the request is not open any
    /// more, or the answer is not one to that kind of request.
    pub fn respond(
        &self,
        client: ClientId,
        request: &RequestId,
        response: ServerResponse,
    ) -> Result<(), OpError> {
        match response {
            ServerResponse::Approval(response) => {
                self.inner.clients.answer(client, request, response)?;
                Ok(())
            }
            _ => Err(error(
                ErrorKind::Usage,
                "request_not_open",
                format!("No request `{request}` of that kind is waiting for an answer."),
            )),
        }
    }

    fn live(&self, session: &SessionId) -> Result<Arc<Live>, OpError> {
        lock(&self.inner.sessions)
            .get(session)
            .cloned()
            .ok_or_else(|| {
                error(
                    ErrorKind::Usage,
                    code::SESSION_NOT_OPEN,
                    format!("The session `{session}` is not open."),
                )
            })
    }

    /// The session, when `client` has it open.
    fn open_for(&self, client: ClientId, session: &SessionId) -> Result<Arc<Live>, OpError> {
        let live = self.live(session)?;
        if live.sink.subscribers().contains(&client.0) {
            Ok(live)
        } else {
            Err(error(
                ErrorKind::Usage,
                code::SESSION_NOT_OPEN,
                format!("The session `{session}` is not open on this connection."),
            ))
        }
    }

    /// Runs blocking work on the blocking pool, through the one seam the
    /// loop's blocking work goes through too (`cortex_tools::run_blocking`):
    /// a test that orders the loop's work orders the core's as well
    /// (`docs/testing-and-ci.md`, "A seeded order of events"). The work is
    /// not cancelled: what the core starts on the pool, it finishes.
    async fn blocking<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, OpError> + Send + 'static,
    ) -> Result<T, OpError> {
        match cortex_tools::run_blocking(&CancelToken::new(), move |_| work()).await {
            Ok(done) => done,
            Err(cortex_tools::BlockingError::Panicked(panic)) => Err(error(
                ErrorKind::Internal,
                "task_failed",
                format!("internal error: the work panicked part-way ({panic})."),
            )),
            Err(cortex_tools::BlockingError::Cancelled) => Err(error(
                ErrorKind::Internal,
                "task_failed",
                "The work was abandoned part-way.",
            )),
        }
    }

    fn not_connected() -> OpError {
        error(
            ErrorKind::Usage,
            code::NOT_INITIALIZED,
            "This client is not connected.",
        )
    }

    fn shutting_down() -> OpError {
        error(
            ErrorKind::Usage,
            code::SHUTTING_DOWN,
            "The server is shutting down, and starts no session.",
        )
    }

    /// Keeps a session that `client` was just put on, if the client is
    /// still there to have it.
    ///
    /// Called after the client is subscribed and the session is in the
    /// table, which is the order [`disconnect`](Self::disconnect) and
    /// [`shutdown`](Self::shutdown) look in the other way round: a client
    /// that left, or a shutdown that began, before this look is seen here,
    /// and one after it sees the session. A session nobody else has open is
    /// then ended, so it never runs with no client.
    async fn still_wanted(&self, client: ClientId, live: &Arc<Live>) -> Result<(), OpError> {
        let gone = self.inner.clients.queue(client).is_none();
        let stopping = self.inner.shutting_down.load(Ordering::SeqCst);
        if !gone && !stopping {
            return Ok(());
        }
        live.sink.unsubscribe(client.0);
        if stopping || live.sink.subscribers().is_empty() {
            self.end(live.handle.id()).await;
        }
        Err(if stopping {
            Self::shutting_down()
        } else {
            Self::not_connected()
        })
    }

    /// Puts a session together and starts its loop.
    async fn open(&self, client: ClientId, wanted: Wanted) -> Result<Arc<Live>, OpError> {
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(Self::shutting_down());
        }
        let Some(queue) = self.inner.clients.queue(client) else {
            return Err(Self::not_connected());
        };
        let inner = Arc::clone(&self.inner);
        let cancel = self.inner.options.cancel.child();
        let assembled = Self::blocking(move || assemble::assemble(&inner, wanted, cancel)).await?;
        let Assembled {
            parts,
            sink,
            log,
            hooks,
            policy,
            board,
            sandbox,
            template,
            loaded,
            images,
            notices,
            lease_count,
        } = assembled;
        // The client is listening before the session can say anything.
        sink.subscribe(client.0, queue);
        let (session, handle) = Session::open(parts)
            .await
            .map_err(|failure| Box::new(cortex_base::Classified::to_error_data(&failure)))?;
        hooks.bind(handle.clone());
        for notice in notices {
            handle.emit(notice);
        }
        // Everything the session's loop logs is inside its span.
        let span = tracing::info_span!("session", session = %handle.id());
        let task = tokio::spawn(tracing::Instrument::instrument(session.run(), span));
        let live = Arc::new(Live {
            handle,
            task: Mutex::new(Some(task)),
            sink,
            log,
            hooks,
            policy,
            board,
            sandbox,
            template: Mutex::new(template),
            loaded,
            images,
            lease_count: AtomicU64::new(lease_count),
        });
        lock(&self.inner.sessions).insert(live.handle.id().clone(), Arc::clone(&live));
        // The client may have left, or a shutdown begun, while the session
        // was put together off this thread.
        self.still_wanted(client, &live).await?;
        Ok(live)
    }

    /// Ends a session: its turn is interrupted, its loop ends, and its log
    /// is closed.
    async fn end(&self, session: &SessionId) {
        let Some(live) = lock(&self.inner.sessions).remove(session) else {
            return;
        };
        live.handle.close();
        let task = lock(&live.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let log = Arc::clone(&live.log);
        let sandbox = Arc::clone(&live.sandbox);
        let closed = Self::blocking(move || {
            // Nothing of the session runs any more: what its commands left
            // in their temporary directory goes.
            sandbox.remove_temp();
            let mut kept = lock(&log);
            if let Some(disk) = kept.disk() {
                // The base state, if its capture ends in time. It nearly
                // always has by now: it takes far less than one request to
                // a model. A capture that is slower than this is let go
                // with the session, which is then not replayable.
                if let Some(state) = disk
                    .base
                    .take()
                    .and_then(|capture| capture.wait_for(BASE_WAIT))
                    && disk.session.is_born()
                {
                    let _ = disk.session.append(None, Body::Base(state));
                }
                let _ = disk.session.end_turn();
            }
            Ok(())
        })
        .await;
        if closed.is_err() {
            tracing::warn!(%session, "the session's log was not closed cleanly");
        }
    }

    /// Ends every session, and starts no more. For the end of the process.
    ///
    /// The mark that no session starts is set first, and the sessions are
    /// looked at after: one that is put together meanwhile either is in the
    /// table by then, and is ended here, or finds the mark when it is put
    /// in the table, and ends itself.
    pub async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let sessions: Vec<SessionId> = lock(&self.inner.sessions).keys().cloned().collect();
        for session in sessions {
            self.end(&session).await;
        }
        // A shell still being asked its version is waited for, so that the
        // answer is kept: each ask is bounded by its own time limit.
        let probes = std::mem::take(&mut *lock(&self.inner.probes));
        if !probes.is_empty() {
            let _ = Self::blocking(move || {
                for probe in probes {
                    let _ = probe.join();
                }
                Ok(())
            })
            .await;
        }
    }

    /// Carries out one operation for `client`.
    ///
    /// # Errors
    ///
    /// The protocol's error object: its kind, a stable code and a message
    /// for a person.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm an operation, each a few lines; splitting it would hide the table"
    )]
    pub async fn handle(&self, client: ClientId, op: Op) -> Result<OpResult, OpError> {
        let not_built = |what: &str, step: &str| -> Result<OpResult, OpError> {
            Err(error(
                ErrorKind::Usage,
                "not_available",
                format!("{what} is not in this build yet: it arrives with {step}."),
            ))
        };
        match op {
            Op::Initialize(params) => {
                let Some(version) = negotiate_version(&params, SUPPORTED_PROTOCOL_VERSIONS) else {
                    return Err(error(
                        ErrorKind::Usage,
                        code::UNSUPPORTED_PROTOCOL_VERSION,
                        format!(
                            "This server speaks protocol version {PROTOCOL_VERSION}, and the \
                             client asked for {}.",
                            params.protocol_version
                        ),
                    ));
                };
                if !params.role.is_known() {
                    return Err(error(
                        ErrorKind::Usage,
                        code::UNKNOWN_VALUE,
                        format!("`{}` is not a client role.", params.role.as_str()),
                    ));
                }
                Ok(OpResult::Initialize(InitializeResult {
                    protocol_version: version,
                    server: PeerInfo {
                        name: "cortex-harness".to_owned(),
                        version: self.inner.options.harness_version.clone(),
                    },
                    capabilities: ServerCapabilities {
                        sessions: true,
                        mcp: false,
                        extensions: false,
                        sandbox: self.inner.sandbox.status(),
                        experimental: Vec::new(),
                    },
                }))
            }
            Op::SessionNew(params) => {
                self.may_drive(client)?;
                let overrides = config_overrides(&params.config)?;
                // `session/set`'s rule, at the start: only a `user` raises a
                // level or loosens what the harness enforces. Anyone else's
                // session starts no higher than it would without them.
                let user =
                    self.inner.clients.info(client).map(|info| info.role) == Some(ClientRole::User);
                if !user {
                    only_preferences(&overrides)?;
                }
                let live = self
                    .open(
                        client,
                        Wanted {
                            which: Which::New {
                                id: self.inner.ids.session_id(),
                                title: params.title,
                                ephemeral: params.ephemeral,
                            },
                            workspace: PathBuf::from(params.workspace),
                            settings: params.settings,
                            overrides,
                            no_raise: !user,
                        },
                    )
                    .await?;
                Ok(OpResult::SessionNew(SessionResult {
                    session: live.handle.state(),
                }))
            }
            Op::SessionOpen(params) => {
                // Open already: this client joins it.
                if let Ok(live) = self.live(&params.session_id) {
                    let Some(queue) = self.inner.clients.queue(client) else {
                        return Err(Self::not_connected());
                    };
                    live.sink.subscribe(client.0, queue);
                    // A client that left in the meantime is not left on it.
                    self.still_wanted(client, &live).await?;
                    // A question that is waiting is put to a client that
                    // may answer it.
                    self.inner.clients.join(&params.session_id, client);
                    return Ok(OpResult::SessionOpen(SessionResult {
                        session: live.handle.state(),
                    }));
                }
                // Starting a stored session writes to it (its lock, its
                // lease), which is more than watching.
                self.may_drive(client)?;
                let workspace = self.stored_workspace(&params.session_id).await?;
                let live = self
                    .open(
                        client,
                        Wanted {
                            which: Which::Resume {
                                id: params.session_id,
                            },
                            workspace,
                            settings: SessionSettings::default(),
                            overrides: Vec::new(),
                            no_raise: false,
                        },
                    )
                    .await?;
                Ok(OpResult::SessionOpen(SessionResult {
                    session: live.handle.state(),
                }))
            }
            Op::SessionList(params) => {
                let sessions = self.list(params.workspace).await?;
                let limit = params.limit.map_or(usize::MAX, |limit| limit as usize);
                let start = params
                    .cursor
                    .as_deref()
                    .and_then(|cursor| cursor.parse::<usize>().ok())
                    .unwrap_or(0);
                let page: Vec<SessionSummary> =
                    sessions.iter().skip(start).take(limit).cloned().collect();
                let next = start + page.len();
                Ok(OpResult::SessionList(SessionListResult {
                    sessions: page,
                    next_cursor: (next < sessions.len()).then(|| next.to_string()),
                }))
            }
            Op::SessionRead(params) => {
                let live = self.open_for(client, &params.session_id)?;
                // The `seq` first: an event after it is not in the answer,
                // and the client gets it live.
                let seq = live.sink.seq();
                let progress = live.sink.in_progress();
                let log = Arc::clone(&live.log);
                let mut items = Self::blocking(move || {
                    lock(&log)
                        .items()
                        .map_err(|problem| error(ErrorKind::Internal, "store_failed", problem))
                })
                .await?;
                // An item that completed between the two looks is in the
                // log already, and is not also given as in progress.
                let incomplete: Vec<ItemId> = progress
                    .iter()
                    .filter_map(|item| item.id().map(ItemId::new))
                    .filter(|id| !items.iter().any(|item| item.id() == Some(id.as_str())))
                    .collect();
                items.extend(progress.into_iter().filter(|item| {
                    item.id()
                        .is_some_and(|id| incomplete.iter().any(|known| known.as_str() == id))
                }));
                if let Some(wanted) = &params.item_id {
                    items.retain(|item| item.id() == Some(wanted.as_str()));
                }
                let start = params
                    .cursor
                    .as_deref()
                    .and_then(|cursor| cursor.parse::<usize>().ok())
                    .unwrap_or(0);
                let limit = params.limit.map_or(usize::MAX, |limit| limit as usize);
                let total = items.len();
                let page: Vec<Item> = items.into_iter().skip(start).take(limit).collect();
                let next = start + page.len();
                let incomplete = incomplete
                    .into_iter()
                    .filter(|id| page.iter().any(|item| item.id() == Some(id.as_str())))
                    .collect();
                Ok(OpResult::SessionRead(SessionReadResult {
                    items: page,
                    incomplete,
                    next_cursor: (next < total).then(|| next.to_string()),
                    seq,
                }))
            }
            Op::SessionFork(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                let id = self.inner.ids.session_id();
                let fork = id.clone();
                let log = Arc::clone(&live.log);
                let options = cortex_session::SessionOptions::new(
                    Arc::clone(&self.inner.options.clock),
                    self.inner.options.harness_version.clone(),
                );
                let workspace = Self::blocking(move || {
                    let mut kept = lock(&log);
                    let Some(disk) = kept.disk() else {
                        return Err(error(
                            ErrorKind::Usage,
                            "session_not_stored",
                            "A session with no log cannot be forked.",
                        ));
                    };
                    let at = match &params.at {
                        Some(item) => {
                            Some(disk.session.entry_of_item(item).cloned().ok_or_else(|| {
                                error(
                                    ErrorKind::Usage,
                                    "unknown_item",
                                    format!("The session has no item `{item}`."),
                                )
                            })?)
                        }
                        None => None,
                    };
                    let (forked, _) = disk
                        .session
                        .fork(at.as_ref(), fork, options)
                        .map_err(|failure| session_error(&failure))?;
                    let workspace = forked.header().workspace.clone();
                    // The fork's lock is let go: it is opened again below,
                    // as any stored session is.
                    drop(forked);
                    Ok(workspace)
                })
                .await?;
                let forked = self
                    .open(
                        client,
                        Wanted {
                            which: Which::Resume { id },
                            workspace: PathBuf::from(workspace),
                            settings: SessionSettings::default(),
                            overrides: Vec::new(),
                            no_raise: false,
                        },
                    )
                    .await?;
                Ok(OpResult::SessionFork(SessionResult {
                    session: forked.handle.state(),
                }))
            }
            Op::SessionRewind(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                // History is not changed under a turn, and no turn starts
                // while it is being changed: the rewind itself runs off
                // this thread, and a prompt could arrive meanwhile. Both
                // are decided under the session's own lock.
                let change = live.handle.change_history()?;
                let log = Arc::clone(&live.log);
                let hooks = Arc::clone(&live.hooks);
                let board = Arc::clone(&live.board);
                let not_restored = Self::blocking(move || {
                    // The change is held by the work, not by this
                    // operation: the work is never cancelled, so if the
                    // operation is given up (its client went away) no turn
                    // starts until the log is no longer being rewritten.
                    let mut change = change;
                    let mut kept = lock(&log);
                    let Some(disk) = kept.disk() else {
                        return Err(error(
                            ErrorKind::Usage,
                            "session_not_stored",
                            "A session with no log cannot be rewound.",
                        ));
                    };
                    let to = disk
                        .session
                        .entry_of_item(&params.to)
                        .cloned()
                        .ok_or_else(|| {
                            error(
                                ErrorKind::Usage,
                                "unknown_item",
                                format!("The session has no item `{}`.", params.to),
                            )
                        })?;
                    let plan = disk
                        .session
                        .plan_rewind(&to)
                        .map_err(|failure| session_error(&failure))?;
                    let restore = if params.restore_files {
                        Restore::files()
                    } else {
                        Restore::conversation_only()
                    };
                    let done = disk
                        .session
                        .rewind(&plan, &restore)
                        .map_err(|failure| session_error(&failure))?;
                    // The table of seen files forgets every file, and the
                    // loop reads the history again before its next turn.
                    change.made();
                    let entries = disk
                        .session
                        .read_branch()
                        .map_err(|failure| session_error(&failure))?;
                    // The plan goes back with the conversation, before a
                    // turn can start on it.
                    hooks.forget_plan_notices();
                    board.restore(crate::store::plan_of(&entries).unwrap_or_default());
                    drop(kept);
                    drop(change);
                    Ok(done.not_restored)
                })
                .await?;
                live.handle.emit(Event::PlanUpdated(PlanUpdated {
                    plan: live.board.get(),
                    by: Actor::Harness,
                }));
                if !not_restored.is_empty() {
                    live.hooks.tell(format!(
                        "These files could not be put back as they were: {}.",
                        not_restored
                            .iter()
                            .map(|(file, why)| format!("{file} ({why})"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                Ok(OpResult::SessionRewind(SessionResult {
                    session: live.handle.state(),
                }))
            }
            Op::SessionClose(params) => {
                if let Ok(live) = self.live(&params.session_id) {
                    live.sink.unsubscribe(client.0);
                    if live.sink.subscribers().is_empty() {
                        self.end(&params.session_id).await;
                    }
                }
                Ok(OpResult::SessionClose(Empty {}))
            }
            Op::TurnStart(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                // Refused with `history_changing` while a rewind is under
                // way, under the session's own lock.
                let result = live.handle.prompt(params.input, params.delivery)?;
                Ok(OpResult::TurnStart(result))
            }
            Op::TurnInterrupt(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                let result = if params.now {
                    live.handle.interrupt_now()
                } else {
                    live.handle.interrupt()
                };
                Ok(OpResult::TurnInterrupt(result))
            }
            Op::SessionCompact(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                live.handle.compact(params.focus)?;
                Ok(OpResult::SessionCompact(Empty {}))
            }
            Op::SessionSet(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                self.set(client, &live, params.settings).await?;
                Ok(OpResult::SessionSet(SessionResult {
                    session: live.handle.state(),
                }))
            }
            Op::PlanGet(params) => {
                let live = self.open_for(client, &params.session_id)?;
                Ok(OpResult::PlanGet(PlanResult {
                    plan: live.board.get(),
                }))
            }
            Op::PlanUpdate(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                let change = live.board.apply(&params.edits).map_err(|problem| {
                    error(ErrorKind::Usage, "invalid_plan_edit", problem.to_string())
                })?;
                if let Some(notice) = change.notice {
                    // The model is told at the next safe boundary, and the
                    // board is told when the loop has recorded that.
                    live.hooks.handed_plan_notice(change.plan.revision);
                    live.handle.notice(NoticeSource::Plan, notice);
                    let log = Arc::clone(&live.log);
                    let plan = change.plan.clone();
                    let recorded = Self::blocking(move || {
                        lock(&log)
                            .record_plan(None, &plan)
                            .map_err(|problem| error(ErrorKind::Internal, "store_failed", problem))
                    })
                    .await;
                    if let Err(problem) = recorded {
                        live.hooks.tell(format!(
                            "The plan was changed, and could not be written to the session: {}",
                            problem.message
                        ));
                    }
                    live.handle.emit(Event::PlanUpdated(PlanUpdated {
                        plan: change.plan.clone(),
                        by: Actor::User,
                    }));
                }
                Ok(OpResult::PlanUpdate(PlanResult { plan: change.plan }))
            }
            Op::UsageGet(params) => {
                let live = self.open_for(client, &params.session_id)?;
                Ok(OpResult::UsageGet(UsageGetResult {
                    usage: live.handle.usage(),
                }))
            }
            Op::UsageReport(_) => not_built("The usage report", "statistics (step 16)"),
            Op::ProjectList(_) | Op::ProjectUpdate(_) => {
                not_built("The project profile", "the project profile (step 17)")
            }
            Op::VerifyRun(_) => not_built("Verification", "verification (step 19)"),
            Op::ModelsList(_) => Ok(OpResult::ModelsList(ModelsListResult {
                models: self
                    .inner
                    .providers
                    .catalog
                    .models()
                    .iter()
                    .map(|entry| entry.info.clone())
                    .collect(),
            })),
            Op::ConfigGet(params) => {
                let loaded = match &params.session_id {
                    Some(session) => Arc::clone(&self.open_for(client, session)?.loaded),
                    None => Arc::clone(&self.inner.user),
                };
                let mut config = serde_json::Map::new();
                for (key, value) in loaded.effective() {
                    let key = key.to_string();
                    if !params.keys.is_empty()
                        && !params
                            .keys
                            .iter()
                            .any(|wanted| key == *wanted || key.starts_with(&format!("{wanted}.")))
                    {
                        continue;
                    }
                    if let Ok(value) = serde_json::to_value(&value) {
                        config.insert(key, value);
                    }
                }
                Ok(OpResult::ConfigGet(ConfigGetResult { config }))
            }
            Op::ConfigReload(_) => not_built(
                "Reloading configuration",
                "the interactive application (step 23)",
            ),
            Op::PermissionsList(params) => {
                let live = self.open_for(client, &params.session_id)?;
                Ok(OpResult::PermissionsList(PermissionsListResult {
                    approvals: live.policy.engine().approvals(),
                }))
            }
            Op::PermissionsRevoke(params) => {
                let live = self.open_for(client, &params.session_id)?;
                self.may_drive(client)?;
                let engine = live.policy.engine();
                Self::blocking(move || {
                    engine.revoke(&params.ids).map_err(|failure| {
                        error(ErrorKind::Internal, "store_failed", failure.to_string())
                    })
                })
                .await?;
                Ok(OpResult::PermissionsRevoke(Empty {}))
            }
            Op::McpStatus(_) => Ok(OpResult::McpStatus(McpStatusResult {
                servers: Vec::new(),
            })),
            Op::ExtensionsStatus(_) => Ok(OpResult::ExtensionsStatus(ExtensionsStatusResult {
                extensions: Vec::new(),
            })),
            Op::Shutdown(_) => {
                self.may_drive(client)?;
                self.shutdown().await;
                Ok(OpResult::Shutdown(Empty {}))
            }
        }
    }

    /// Whether the client may change a session, start one or end the
    /// core: an observer only watches.
    fn may_drive(&self, client: ClientId) -> Result<(), OpError> {
        match self.inner.clients.info(client).map(|info| info.role) {
            Some(ClientRole::User | ClientRole::Supervisor) => Ok(()),
            _ => Err(error(
                ErrorKind::Usage,
                code::ROLE_NOT_ALLOWED,
                "An `observer` client watches a session and cannot change it.",
            )),
        }
    }

    /// Changes a session's settings.
    ///
    /// Everything asked for is checked and made ready before anything is
    /// changed, so a request that is refused in any part changes nothing.
    async fn set(
        &self,
        client: ClientId,
        live: &Arc<Live>,
        settings: SessionSettings,
    ) -> Result<(), OpError> {
        let inner = &self.inner;
        let has_history = live.handle.has_history();
        let mut uncached = false;
        let mut told = Vec::new();

        // ------------------------------------------------------ checked
        let mut new_level = None;
        if let Some(level) = settings.level {
            if !level.is_known() {
                return Err(error(
                    ErrorKind::Usage,
                    code::UNKNOWN_VALUE,
                    format!("`{}` is not a level.", level.as_str()),
                ));
            }
            let current = live.policy.engine().level().clone();
            if level != current {
                let raises = !cortex_policy::level::no_higher_than(&level, &current);
                let role = inner.clients.info(client).map(|info| info.role);
                if raises && role != Some(ClientRole::User) {
                    return Err(error(
                        ErrorKind::Permission,
                        code::ROLE_NOT_ALLOWED,
                        "Only a `user` client may raise a session's level.",
                    ));
                }
                new_level = Some(level);
            }
        }

        let mut roles = Vec::new();
        for (role, change) in settings.roles {
            let speed = change.speed.clone();
            if change.model.is_none() && change.effort.is_none() && change.provider.is_none() {
                roles.push((role, speed, None));
                continue;
            }
            let current = live.handle.role_model(&role);
            let model_settings = &live.loaded.settings.model;
            // What is not named stays: the model, and the level as it was
            // set (not as it was mapped, which belongs to the old model).
            let entry = match (&change.model, &current) {
                (Some(model), _) => inner.providers.entry(
                    model.as_str(),
                    change
                        .provider
                        .as_ref()
                        .map(cortex_protocol::ProviderId::as_str),
                )?,
                (None, Some(current)) => match &change.provider {
                    Some(provider) => inner
                        .providers
                        .entry(current.entry.info.id.as_str(), Some(provider.as_str()))?,
                    None => Arc::clone(&current.entry),
                },
                (None, None) => {
                    return Err(error(
                        ErrorKind::Usage,
                        "no_model",
                        format!(
                            "The role `{}` has no model: name one to give it a reasoning level.",
                            role.as_str()
                        ),
                    ));
                }
            };
            let named = change.effort.clone();
            let carried = current.as_ref().and_then(|model| model.effort.clone());
            let max_output = current
                .as_ref()
                .map_or(model_settings.max_output, |model| model.max_output);
            let settings = live.loaded.settings.clone();
            let env = inner.options.env.clone();
            let images = Arc::clone(&live.images);
            let core = Arc::clone(inner);
            // Finding a key can read the credential store.
            let (model, left) = Self::blocking(move || {
                let make = |effort: Option<&str>| {
                    core.providers.role_model(
                        &settings,
                        &env,
                        Arc::clone(&entry),
                        effort,
                        max_output,
                        Arc::clone(&images),
                    )
                };
                match (named, carried) {
                    // A level that was asked for is refused if the model
                    // has no such level.
                    (Some(named), _) => make(Some(&named)).map(|model| (model, None)),
                    // One that is only carried over from the model before
                    // is left behind where the new model has no such
                    // level: nobody asked for it here.
                    (None, Some(carried)) => match make(Some(&carried)) {
                        Err(refused) if refused.code == "unknown_effort" => {
                            make(None).map(|model| (model, Some(carried)))
                        }
                        made => made.map(|model| (model, None)),
                    },
                    (None, None) => make(None).map(|model| (model, None)),
                }
            })
            .await?;
            if let Some(left) = left {
                told.push(format!(
                    "`{}` has no reasoning level `{left}`, so the role `{}` runs at the model's \
                     own default.",
                    model.entry.info.id,
                    role.as_str()
                ));
            }
            roles.push((role, speed, Some(model)));
        }

        // ------------------------------------------------------ applied
        // The level first: it is the one change that can still fail (the
        // level cannot be held, or its lease cannot be written), and then
        // nothing else has been changed.
        if let Some(level) = new_level {
            self.set_level(live, level).await?;
        }

        if let Some(speed) = settings.speed {
            let model = live.handle.role_model(&ModelRole::Main);
            let shares = model
                .as_ref()
                .and_then(|model| model.entry.info.speed_offer(&speed))
                .and_then(|offer| offer.shares_cache);
            if live.handle.state().speed != speed && shares != Some(true) {
                uncached = true;
            }
            live.handle.set_speed(speed);
        }

        for (role, speed, model) in roles {
            if role == ModelRole::Main && (speed.is_some() || model.is_some()) {
                uncached = true;
            }
            if let Some(speed) = speed {
                live.handle.set_role_speed(role.clone(), speed);
            }
            if let Some(model) = model {
                live.handle.set_role_model(role, model);
            }
        }

        for notice in told {
            live.hooks.tell(notice);
        }
        if uncached && has_history {
            live.hooks.tell(UNCACHED);
        }
        Ok(())
    }

    /// Puts the session at another level: a new engine with the same
    /// rules, and a new standing lease in the log.
    async fn set_level(&self, live: &Arc<Live>, level: Level) -> Result<(), OpError> {
        let old = live.policy.engine();
        let number = live.lease_count.fetch_add(1, Ordering::Relaxed) + 1;
        let lease_id = LeaseId::new(format!("l_{number}"));
        let mut parts = lock(&live.template).parts.clone();
        parts.level = level.clone();
        parts.lease_id = lease_id.clone();
        // What was approved so far stays approved, each as what it is:
        // what is stored for the project is read as stored, and what was
        // allowed for the session is taken over as the session's own.
        parts.approved = old
            .approvals()
            .into_iter()
            .filter(|approval| approval.scope == ApprovalScope::Project)
            .collect();
        let engine = assemble::start_engine(parts.clone())?;
        engine.keep_session_approvals_of(&old);
        let (ended, granted) = (old.lease().clone(), engine.lease().clone());
        let notice = engine.sandbox_notice();
        let log = Arc::clone(&live.log);
        Self::blocking(move || {
            let mut kept = lock(&log);
            if let Some(disk) = kept.disk() {
                for (event, lease) in [(LeaseEvent::Ended, ended), (LeaseEvent::Granted, granted)] {
                    disk.session
                        .append(None, Body::Lease(LeaseRecord { event, lease }))
                        .map_err(|failure| session_error(&failure))?;
                }
            }
            Ok(())
        })
        .await?;
        lock(&live.template).parts = parts;
        live.policy.replace(engine);
        // Nothing is confined at `unrestricted`, and everything is again
        // below it.
        live.sandbox.set_level(&level);
        live.policy.set_sandboxed(live.sandbox.confines());
        live.handle.set_sandbox(if live.sandbox.confines() {
            SandboxStatus::Enforcing
        } else {
            SandboxStatus::Off
        });
        live.handle.set_level(level, Some(lease_id));
        if let Some(message) = notice {
            live.hooks
                .emit(Event::Notice(cortex_protocol::Notice::Sandbox(
                    cortex_protocol::event::SandboxNotice {
                        message,
                        status: SandboxStatus::Off,
                    },
                )));
        }
        Ok(())
    }

    /// Where the sessions of every project are kept.
    fn sessions_root(&self) -> Result<SessionsRoot, OpError> {
        let data = self.inner.user.data_dir().ok_or_else(|| {
            error(
                ErrorKind::Config,
                "no_data_dir",
                "There is nowhere sessions are kept: no data directory could be worked out.",
            )
        })?;
        Ok(SessionsRoot::in_data_dir(&data))
    }

    /// The stored sessions of a workspace, or of every workspace, newest
    /// first.
    async fn list(&self, workspace: Option<String>) -> Result<Vec<SessionSummary>, OpError> {
        let root = self.sessions_root()?;
        Self::blocking(move || {
            let projects: Vec<cortex_session::ProjectSessions> = match workspace {
                Some(workspace) => {
                    let Some(resolved) = assemble::resolve_dir(std::path::Path::new(&workspace))
                    else {
                        return Ok(Vec::new());
                    };
                    root.project(&resolved.project_key()).into_iter().collect()
                }
                None => match std::fs::read_dir(root.path()) {
                    Ok(entries) => entries
                        .flatten()
                        .map(|entry| cortex_session::ProjectSessions::at(entry.path()))
                        .collect(),
                    Err(_) => Vec::new(),
                },
            };
            let mut sessions: Vec<SessionSummary> = Vec::new();
            for project in projects {
                if let Ok(records) = project.index().list() {
                    sessions.extend(records.iter().map(SessionSummary::from));
                }
            }
            sessions.sort_by(|a, b| b.session_id.cmp(&a.session_id));
            Ok(sessions)
        })
        .await
    }

    /// The workspace a stored session was made in.
    async fn stored_workspace(&self, session: &SessionId) -> Result<PathBuf, OpError> {
        let wanted = session.clone();
        let found = self
            .list(None)
            .await?
            .into_iter()
            .find(|summary| summary.session_id == wanted);
        found
            .map(|summary| PathBuf::from(summary.workspace))
            .ok_or_else(|| {
                error(
                    ErrorKind::Usage,
                    code::UNKNOWN_SESSION,
                    format!("There is no stored session `{session}`."),
                )
            })
    }

    /// Where the state directory is: the trust store, stored approvals and
    /// log files. `None` when no home directory could be worked out.
    #[must_use]
    pub fn state_dir(&self) -> Option<StateDir> {
        StateDir::from_dirs(&self.inner.user.dirs)
    }
}

fn session_error(failure: &cortex_session::SessionError) -> OpError {
    Box::new(cortex_base::Classified::to_error_data(failure))
}

/// Refuses a session's configuration overrides from a client that is not a
/// `user` when they set anything but a preference, or a choice that raises
/// what is spent (a model, a speed: what `session/set` lets such a client
/// choose). Such overrides rank as the user's own command line, so the
/// rest is the user's to give:
///
/// - anything under `[permissions]`, `[sandbox]` or `[limits]`, in a
///   profile too, whatever its value;
/// - what is the user's alone: a provider's address and headers, the
///   shell, the data directory, the credential store;
/// - what runs code or widens access.
///
/// Each key is judged by the rule the schema gives it for a project's file.
/// (Choosing a profile is not an override anyone may give: the loader
/// refuses it.)
fn only_preferences(overrides: &[Override]) -> Result<(), OpError> {
    use cortex_config::Trust;
    use cortex_config::schema::{Lookup, Meta, lookup};
    let preference = |trust: &Trust| {
        matches!(
            trust,
            Trust::Preference | Trust::RaisesCost | Trust::BySpeed
        )
    };
    for one in overrides {
        let allowed = match cortex_config::KeyPath::parse(&one.key) {
            Ok(path) => match lookup(&path) {
                Lookup::Setting { child, enforced } => {
                    !enforced
                        && matches!(&child.meta, Meta::Leaf { trust, .. } if preference(trust))
                }
                Lookup::InsideSetting { child } => {
                    matches!(&child.meta, Meta::Leaf { trust, .. } if preference(trust))
                }
                Lookup::Reserved { trust } => preference(&trust),
                Lookup::Table { .. } | Lookup::Unknown { .. } => false,
            },
            // A key that cannot be read cannot be shown to be harmless.
            Err(_) => false,
        };
        if !allowed {
            return Err(error(
                ErrorKind::Permission,
                code::ROLE_NOT_ALLOWED,
                format!(
                    "Only a `user` client may set `{}` for a session: permissions, the sandbox, \
                     limits and what is the user's alone are the user's.",
                    one.key
                ),
            ));
        }
    }
    Ok(())
}

/// A session's configuration overrides, as the configuration file would
/// have them, flattened into the settings they set.
fn config_overrides(
    config: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<Override>, OpError> {
    fn walk(
        prefix: &str,
        value: &serde_json::Value,
        overrides: &mut Vec<Override>,
    ) -> Result<(), OpError> {
        if let serde_json::Value::Object(fields) = value {
            for (name, value) in fields {
                let key = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                walk(&key, value, overrides)?;
            }
            return Ok(());
        }
        let value = toml::Value::try_from(value).map_err(|problem| {
            error(
                ErrorKind::Config,
                "invalid_configuration",
                format!("`{prefix}` cannot be a configuration value: {problem}"),
            )
        })?;
        overrides.push(Override {
            key: prefix.to_owned(),
            value: value.to_string(),
            flag: None,
        });
        Ok(())
    }
    let mut overrides = Vec::new();
    walk(
        "",
        &serde_json::Value::Object(config.clone()),
        &mut overrides,
    )?;
    Ok(overrides)
}
