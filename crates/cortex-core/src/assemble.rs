//! Putting one session together: configuration, trust, the permission
//! engine, the models, the store and the tools, each from the crate that
//! owns it (`docs/architecture/00-overview.md`).
//!
//! Everything here blocks (it reads configuration, the trust store, the
//! session's files, and looks for a shell), so the core runs it on the
//! blocking pool and opens the session's loop with what comes back.
//!
//! # Trust, worked out once
//!
//! Whether the project is trusted is worked out in one place ([`trust`]),
//! and it is two answers, because trust lets a project do two different
//! kinds of thing:
//!
//! - **Loosen what is asked, or raise what is spent** ([`Trust::rules`]):
//!   given to configuration (which holds back what a project may only set
//!   once trusted) and to the permission engine (which sets a project's
//!   `allow` rules aside until then). This answer is yes only while the
//!   project's rules are the ones the user saw: any change to them sets it
//!   aside until the change is approved.
//! - **Have git read the repository's own configuration**
//!   ([`Trust::repository`]): given to the engine (which takes git's
//!   commands for ones that only read in a trusted repository alone,
//!   because a repository's configuration can make git run a program), to
//!   the shell tool (which asks git what a command changed, under the same
//!   condition) and to the capture of the base state (the same again). A change to the
//!   project's permission rules does not touch what git would run, so
//!   this answer is the user's trust alone: a project that adds a `deny`
//!   does not lose its base state for it.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "assembling a session reads files, on the blocking pool"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use cortex_agent::{
    Account, ContextParts, ContextSettings, Environment, Ledger, LoopConfig, Models,
    NoInstructionFiles, NoStats, NoVerifier, SessionInfo, SessionParts, SpeedSettings, Workspace,
};
use cortex_base::{AbsPath, CancelToken, PathOptions, PathResolver};
use cortex_config::{LayerKind, LoadOptions, Loaded, Override, Settings, StateDir};
use cortex_llm::{BoxFuture, ImageError, ImageSource};
use cortex_policy::{
    ApprovalStore, Engine, NoSandbox, Parts, Protected, RuleSource, SandboxState, ShellSyntax,
    SourcedRule, StartError, TrustStore, default_sandbox,
};
use cortex_protocol::event::{Event, Notice, SandboxNotice};
use cortex_protocol::lease::{Budget, HardLimits, Rule};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::SessionSettings;
use cortex_protocol::{ArtifactId, ErrorData, ErrorKind, LeaseId, Plan, SessionId};
use cortex_session::entry::{LeaseEvent, LeaseRecord, Meta};
use cortex_session::{
    BaseCapture, BaseLimits, Body, Durability, Git, KEEP_LIMIT, SessionError, SessionOptions,
    SessionsRoot, SystemGit,
};
use cortex_tools::process::{Guard, Runner};
use cortex_tools::{
    ContentStore, EditTool, Editing, FileTable, MemoryCheckpoints, MemoryProposals, MemoryStore,
    OutputLimits, PlanBoard, PlanTool, RememberTool, Shell, ShellConfig, ShellSearch, ShellTool,
    ToolRegistry, Toolbox, WriteTool,
};

use crate::checkpoints::{RecordingShell, Rounds, StopsWithTurn};
use crate::content::{DiskContent, StoredImages};
use crate::core::Inner;
use crate::events::SessionSink;
use crate::instructions::DiskInstructions;
use crate::policy::{EnginePolicy, PolicyParts};
use crate::provider;
use crate::redact::LogRedactor;
use crate::sandbox::{self, SessionGit, SessionSandbox, Temp, status_of};
use crate::shell_cache;
use crate::store::{Disk, Hooks, Kept, LogStore, SharedLog};
use cortex_tools::shell::ShellKind;

/// Whether the workspace is in a git repository: a `.git` in it or in a
/// directory above it. Looked at, not asked of git, which starts nothing.
fn in_git(root: &AbsPath) -> bool {
    root.to_fs_path()
        .ancestors()
        .any(|dir| std::fs::symlink_metadata(dir.join(".git")).is_ok())
}

/// The context's settings, from `[context]` and `compat.instructions`.
fn context_settings(settings: &Settings) -> ContextSettings {
    let context = &settings.context;
    let mut names: Vec<String> = cortex_agent::context::DEFAULT_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    // Only plain documents: a name with a separator, or one that is not a
    // `.md` file, could make the harness read and send something that is
    // not instructions (`cortex_agent::context::is_instruction_name`).
    for name in &context.instruction_names {
        if cortex_agent::context::is_instruction_name(name) && !names.contains(name) {
            names.push(name.clone());
        }
    }
    ContextSettings {
        auto_compact: context.auto_compact,
        background: context.background,
        compact_at: context.compact_at,
        prepare_at: context.prepare_at,
        keep_recent_tokens: context.keep_recent_tokens,
        instruction_names: names,
        instructions_cap: usize::try_from(context.instructions_cap).unwrap_or(usize::MAX),
        ..ContextSettings::default()
    }
}

fn error(kind: ErrorKind, code: &str, message: impl Into<String>) -> Box<ErrorData> {
    Box::new(ErrorData::new(kind, code, message))
}

fn session_error(error: &SessionError) -> Box<ErrorData> {
    Box::new(cortex_base::Classified::to_error_data(error))
}

/// Which session to put together.
#[derive(Debug, Clone)]
pub(crate) enum Which {
    /// A new one, with this id.
    New {
        id: SessionId,
        title: Option<String>,
        ephemeral: bool,
    },
    /// A stored one.
    Resume { id: SessionId },
}

/// What a session is asked to be.
#[derive(Debug, Clone)]
pub(crate) struct Wanted {
    pub(crate) which: Which,
    pub(crate) workspace: PathBuf,
    pub(crate) settings: SessionSettings,
    /// Overrides for this session, over the core's own.
    pub(crate) overrides: Vec<Override>,
    /// Whether the session may not start at a higher level than it would
    /// without `settings` and `overrides`: they come from a client that is
    /// not a `user`, which `session/set` would not let raise the level.
    pub(crate) no_raise: bool,
}

/// The images of a session's store, which exists only after the clients
/// that will ask for them.
#[derive(Default)]
struct LateImages(OnceLock<StoredImages>);

impl std::fmt::Debug for LateImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LateImages").finish_non_exhaustive()
    }
}

impl ImageSource for LateImages {
    fn load<'a>(&'a self, artifact: &'a ArtifactId) -> BoxFuture<'a, Result<Vec<u8>, ImageError>> {
        match self.0.get() {
            Some(images) => images.load(artifact),
            None => Box::pin(std::future::ready(Err(ImageError::new(
                "the session's store is not open",
            )))),
        }
    }
}

/// What the engine was started from, kept so that it can be started again
/// at another level.
#[derive(Debug, Clone)]
pub(crate) struct EngineTemplate {
    pub(crate) parts: Parts,
}

/// A session, put together and not yet running.
pub(crate) struct Assembled {
    pub(crate) parts: SessionParts,
    pub(crate) sink: Arc<SessionSink>,
    pub(crate) log: SharedLog,
    pub(crate) hooks: Arc<Hooks>,
    pub(crate) policy: Arc<EnginePolicy>,
    pub(crate) board: Arc<PlanBoard>,
    pub(crate) sandbox: Arc<SessionSandbox>,
    pub(crate) template: EngineTemplate,
    pub(crate) loaded: Arc<Loaded>,
    pub(crate) images: Arc<dyn ImageSource>,
    /// What to tell front ends once the session is open.
    pub(crate) notices: Vec<Event>,
    pub(crate) lease_count: u64,
}

/// Where a directory really is: resolved through any link, whether or not
/// all of it exists yet.
pub(crate) fn resolve_dir(path: &Path) -> Option<AbsPath> {
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(resolver) = PathResolver::new(existing, PathOptions::new()) {
            let mut resolved = resolver.root().clone();
            for name in missing.iter().rev() {
                resolved = resolved.child(name).ok()?;
            }
            return Some(resolved);
        }
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}

/// The source of a rule the configuration holds, by the layer it is in.
fn rule_source(layer: LayerKind, defined_in: Option<LayerKind>) -> RuleSource {
    match layer {
        LayerKind::Managed => RuleSource::Managed,
        LayerKind::Project | LayerKind::ProjectLocal => RuleSource::Project,
        LayerKind::CommandLine => RuleSource::CommandLine,
        // A profile is the user's unless a project's file defines it.
        LayerKind::Profile => match defined_in {
            Some(LayerKind::Project | LayerKind::ProjectLocal) => RuleSource::Project,
            Some(LayerKind::Managed) => RuleSource::Managed,
            _ => RuleSource::User,
        },
        _ => RuleSource::User,
    }
}

/// Every permission rule of a loaded configuration, with who wrote it.
fn sourced_rules(loaded: &Loaded) -> Vec<SourcedRule> {
    loaded
        .rules()
        .into_iter()
        .map(|placed| {
            let source = rule_source(placed.origin.layer, placed.origin.defined_in);
            SourcedRule::new(placed.rule, source)
        })
        .collect()
}

/// The project's own permission rules, every action of them, as a load
/// that trusts the project holds them: the set the trust store pins
/// (`cortex permissions trust`) and a start compares with the pin.
pub(crate) fn project_rules(loaded: &Loaded) -> Vec<Rule> {
    sourced_rules(loaded)
        .into_iter()
        .filter(|rule| rule.source == RuleSource::Project)
        .map(|rule| rule.rule)
        .collect()
}

/// Whether the project is trusted for the rules its files hold now.
///
/// The one place the trust store's answer about a project's rules is
/// asked for: every consumer of trust is given what this returns.
fn project_rules_approved(trust: &cortex_policy::ProjectTrust, project_rules: &[Rule]) -> bool {
    trust.rules_approved(project_rules)
}

/// What was learned about the project's trust.
struct Trust {
    /// Whether the project may loosen what is asked and raise what is
    /// spent: it is trusted, and its rules are the ones the user saw.
    rules: bool,
    /// Whether git may read the repository's own configuration: the user
    /// trusts the project.
    repository: bool,
    loaded: Loaded,
    /// What went wrong reading the trust store, for the user.
    problem: Option<String>,
}

/// Works out whether the project is trusted, and loads its configuration
/// with that answer.
fn trust(
    inner: &Inner,
    state: Option<&StateDir>,
    root: &AbsPath,
    options: &LoadOptions,
) -> Result<Trust, Box<ErrorData>> {
    let load = |trusted: bool| {
        let mut options = options.clone();
        options.trust = if trusted {
            cortex_config::ProjectTrust::Trusted
        } else {
            cortex_config::ProjectTrust::Untrusted
        };
        cortex_config::load(&options).map_err(|failure| {
            Box::new(ErrorData::new(
                ErrorKind::Config,
                "invalid_configuration",
                failure.render(&inner.options.env),
            ))
        })
    };
    if inner.options.trust_project {
        // The user's own command line says so, for this run.
        return Ok(Trust {
            rules: true,
            repository: true,
            loaded: load(true)?,
            problem: None,
        });
    }
    let (stored, problem) = match state {
        Some(state) => {
            let store = TrustStore::new(state.trust_file(), Arc::clone(&inner.options.clock));
            match store.project(&root.project_key()) {
                Ok(trust) => (trust, None),
                // A trust store that cannot be read trusts nothing.
                Err(error) => (
                    cortex_policy::ProjectTrust::untrusted(),
                    Some(format!(
                        "The trust store could not be read, so this project is treated as not \
                         trusted: {error}"
                    )),
                ),
            }
        }
        None => (cortex_policy::ProjectTrust::untrusted(), None),
    };
    if !stored.is_trusted() {
        return Ok(Trust {
            rules: false,
            repository: false,
            loaded: load(false)?,
            problem,
        });
    }
    // Trusted when the user last looked. The rules the project's files hold
    // now are read as a trusted project's would be, so that none is held
    // back before it is compared with what the user saw.
    let peek = load(true)?;
    if project_rules_approved(&stored, &project_rules(&peek)) {
        Ok(Trust {
            rules: true,
            repository: true,
            loaded: peek,
            problem,
        })
    } else {
        // The user still trusts the project; what they have not seen is
        // its rules as they are now. What the rules could loosen is set
        // aside, and what has nothing to do with them is not.
        Ok(Trust {
            rules: false,
            repository: true,
            loaded: load(false)?,
            problem: Some(
                "This project's permission rules have changed since it was trusted, so its \
                 settings and its allow rules are set aside until the change is approved."
                    .to_owned(),
            ),
        })
    }
}

/// The session's settings as configuration overrides, which rank as the
/// command line does: they are the user's own.
fn settings_overrides(settings: &SessionSettings) -> Vec<Override> {
    let text = |value: &str| toml::Value::String(value.to_owned()).to_string();
    let mut overrides = Vec::new();
    let mut set = |key: String, value: String| {
        overrides.push(Override {
            key,
            value,
            flag: None,
        });
    };
    if let Some(level) = &settings.level {
        set("permissions.mode".to_owned(), text(level.as_str()));
    }
    if let Some(speed) = &settings.speed {
        set("model.speed".to_owned(), text(speed.as_str()));
    }
    for (role, role_settings) in &settings.roles {
        let role = role.as_str();
        if let Some(model) = &role_settings.model {
            set(format!("model.{role}"), text(model.as_str()));
        }
        if let Some(provider) = &role_settings.provider {
            set(
                format!("model.roles.{role}.provider"),
                text(provider.as_str()),
            );
        }
        if let Some(effort) = &role_settings.effort {
            set(format!("model.roles.{role}.effort"), text(effort));
        }
        if let Some(speed) = &role_settings.speed {
            set(format!("model.roles.{role}.speed"), text(speed.as_str()));
        }
    }
    overrides
}

/// The budget a set of limits is, in the lease's terms.
pub(crate) fn budget(limits: &cortex_config::schema::LimitSet) -> Budget {
    Budget {
        usd_nanos: limits.cost_usd_nanos(),
        tokens: limits.tokens,
        requests: limits.requests,
        wall_clock_ms: limits
            .time
            .map(|time| u64::try_from(time.as_millis()).unwrap_or(u64::MAX)),
    }
}

/// The parts of an engine for a session in `root`.
#[expect(
    clippy::too_many_arguments,
    reason = "each is one thing the engine is made from"
)]
fn engine_parts(
    inner: &Inner,
    loaded: &Loaded,
    state: Option<&StateDir>,
    resolver: &Arc<PathResolver>,
    session: &SessionId,
    lease_id: LeaseId,
    trusted: bool,
    repository_trusted: bool,
    shell: ShellSyntax,
    notices: &mut Vec<String>,
) -> Parts {
    let settings = &loaded.settings;
    let root = resolver.root().clone();
    let env = &inner.options.env;
    let home_dir = env.home();
    let home = home_dir.as_deref().and_then(resolve_dir);

    let mut rules = sourced_rules(loaded);
    rules.extend(
        inner
            .options
            .rules
            .iter()
            .cloned()
            .map(|rule| SourcedRule::new(rule, RuleSource::CommandLine)),
    );

    let store = state
        .and_then(|state| state.approvals_file(&root.project_key()))
        .map(|file| ApprovalStore::new(file, Arc::clone(&inner.options.clock)));
    let approved = match store.as_ref().map(ApprovalStore::load) {
        Some(Ok(approved)) => approved,
        Some(Err(error)) => {
            // An approvals file that cannot be read approves nothing.
            notices.push(format!(
                "The stored approvals for this project could not be read, so none applies: \
                 {error}"
            ));
            Vec::new()
        }
        None => Vec::new(),
    };

    // A path the user wrote, as where it really is. One that cannot be
    // placed is passed on as written, for the engine to refuse by name.
    let place = |entry: &String| -> String {
        let expanded = cortex_config::load::expand_home(entry, home_dir.as_deref());
        let absolute = match expanded {
            Some(path) if path.is_absolute() => Some(path),
            Some(path) => Some(root.to_fs_path().join(path)),
            None => None,
        };
        absolute
            .as_deref()
            .and_then(resolve_dir)
            .map_or_else(|| entry.clone(), |resolved| resolved.as_str().to_owned())
    };
    let temporary = sandbox::temp_path(session);
    let mut writable: Vec<String> = resolve_dir(&temporary)
        .map(|path| path.as_str().to_owned())
        .into_iter()
        .collect();
    writable.extend(settings.sandbox.write.iter().map(place));
    let protect: Vec<String> = settings.sandbox.protect.iter().map(place).collect();

    let mut protected = Protected::none();
    let dirs = &loaded.dirs;
    if let Some(config) = dirs.config.as_deref().and_then(resolve_dir) {
        protected = protected.closed(config);
    }
    // The stored keys, wherever they are kept: in the configuration
    // directory on Linux, and on Windows in a folder of their own beside the
    // data directory's sessions. The folder is closed whole: the file, its
    // lock, and a temporary file a write that was killed left behind.
    if let Some(folder) = dirs.credentials.as_deref().and_then(resolve_dir) {
        protected = protected.closed(folder);
    }
    // The user's configuration file, which `CORTEX_CONFIG` can put outside
    // the configuration directory: it holds the level and the rules the
    // next session starts with, and can hold a provider's header. Closed
    // as named and, when it is a link (a dotfiles repository), where it
    // leads, which is what a read or a write of it reaches.
    if let Some(file) = dirs.user_config.as_deref() {
        if let Some(named) = resolve_dir(file) {
            protected = protected.closed(named);
        }
        if let Some(target) = std::fs::canonicalize(file)
            .ok()
            .and_then(|target| AbsPath::from_os(&target).ok())
        {
            protected = protected.closed(target);
        }
    }
    if let Some(state) = state.and_then(|state| resolve_dir(state.root())) {
        protected = protected.closed(state);
    }
    if let Some(data) = loaded.data_dir().as_deref().and_then(resolve_dir) {
        let sessions = SessionsRoot::in_data_dir(&data.to_fs_path());
        protected = protected.unwritable(data);
        if let Some(sessions) = resolve_dir(sessions.path()) {
            protected = protected.other_sessions(sessions, root.project_key());
        }
    }

    Parts {
        lease_id,
        level: settings.permissions.mode.clone(),
        rules,
        approved,
        sandbox_policy: {
            let mut policy = default_sandbox(&root, &writable, &protect);
            policy.network = settings.sandbox.network;
            policy
        },
        sandbox: inner.sandbox.state(settings.sandbox.enabled).0,
        models: Vec::new(),
        budget: budget(&settings.limits.session),
        limits: HardLimits::default(),
        workspace: root,
        home,
        trusted,
        repository_trusted,
        protected,
        shell,
        locate: Arc::clone(resolver) as Arc<dyn cortex_policy::Locate>,
        store,
        clock: Arc::clone(&inner.options.clock),
    }
}

/// Whether a write root of `policy` holds the harness's configuration,
/// state or data directory, or is one, or lies inside one; or holds the
/// user's configuration file, as named or where a link leads (a dotfiles
/// repository), which holds the level and rules the next session starts
/// with; or holds the program started as the launcher, which a confined
/// command could then replace with one that confines nothing (found by the
/// second review of step 26).
fn holds_harness_files(
    policy: &cortex_protocol::lease::SandboxPolicy,
    loaded: &Loaded,
    state: Option<&StateDir>,
    launcher: Option<&Path>,
) -> bool {
    // Resolved paths against the lease's roots, which are resolved too, in
    // one spelling; names compared as the system compares them, folding
    // case on Windows, since a match here only takes the sandbox away.
    let own: Vec<AbsPath> = [
        loaded.dirs.config.clone(),
        state.map(|state| state.root().to_path_buf()),
        loaded.data_dir(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|dir| resolve_dir(&dir))
    .collect();
    // The launcher's program and the user's configuration file, each as
    // it is named and as it really is: its directory resolved, and its own
    // name.
    let files: Vec<AbsPath> = launcher
        .into_iter()
        .chain(loaded.dirs.user_config.as_deref())
        .flat_map(|program| {
            [
                Some(program.to_path_buf()),
                std::fs::canonicalize(program).ok(),
            ]
        })
        .flatten()
        .filter_map(|program| {
            let name = program.file_name()?.to_owned();
            resolve_dir(program.parent()?)?.child(&name).ok()
        })
        .collect();
    policy.write.all
        || policy
            .write
            .roots
            .iter()
            .filter_map(|root| AbsPath::parse(root, cortex_base::PathStyle::NATIVE).ok())
            .any(|root| {
                own.iter()
                    .any(|dir| dir.starts_with(&root) || root.starts_with(dir))
                    || files.iter().any(|file| file.starts_with(&root))
            })
}

/// Starts an engine, with its failure as the error a front end is shown.
pub(crate) fn start_engine(parts: Parts) -> Result<Engine, Box<ErrorData>> {
    Engine::start(parts).map_err(|failure| match failure {
        // The level cannot be held here: the session does not start.
        StartError::Level(level) => Box::new(level.to_error_data()),
        other => error(ErrorKind::Config, "invalid_permissions", other.to_string()),
    })
}

/// The models of a session, by role, from its settings.
fn models(
    inner: &Inner,
    settings: &Settings,
    images: &Arc<dyn ImageSource>,
) -> Result<Models, Box<ErrorData>> {
    let env = &inner.options.env;
    let providers = &inner.providers;
    let role = |role: &ModelRole, name: &str| {
        let entry = providers.entry(name, settings.model.provider_for(role))?;
        providers.role_model(
            settings,
            env,
            entry,
            settings.model.effort_for(role),
            // The output limit is the main role's: a utility role's
            // requests set their own.
            (*role == ModelRole::Main)
                .then_some(settings.model.max_output)
                .flatten(),
            Arc::clone(images),
        )
    };
    let Some(main) = settings.model.model_for(&ModelRole::Main) else {
        return Err(error(
            ErrorKind::Config,
            "no_model",
            "No model is set. Set `model.main` in the configuration, or name one with `--model`.",
        ));
    };
    let mut models = Models::new(
        role(&ModelRole::Main, main)?,
        providers.catalog.version().to_owned(),
    );
    for utility in [ModelRole::Fast, ModelRole::Judge] {
        if let Some(name) = settings.model.model_for(&utility) {
            let model = role(&utility, name)?;
            models = models.with_role(utility, model);
        }
    }
    Ok(models)
}

/// The speeds a session starts at.
fn speeds(settings: &Settings) -> SpeedSettings {
    let mut speeds = SpeedSettings {
        session: settings.model.speed.clone(),
        ..SpeedSettings::default()
    };
    for (role, role_settings) in &settings.model.roles {
        if let Some(speed) = &role_settings.speed {
            speeds
                .roles
                .insert(ModelRole::from_wire(role), speed.clone());
        }
    }
    speeds
}

/// The shell a session's commands run in: the one the options give, or
/// the one found on this machine, asked its version the first time.
fn find_shell(
    inner: &Inner,
    settings: &Settings,
) -> Result<Shell, cortex_tools::shell::ShellError> {
    if let Some(shell) = &inner.options.shell {
        return Ok(shell.clone());
    }
    let key = (settings.shell.program.clone(), settings.shell.args.clone());
    let known = inner
        .shells
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned();
    if let Some(shell) = known {
        return Ok(shell);
    }
    let mut shell = ShellSearch::from_env().find(key.0.as_deref(), key.1.as_deref())?;
    // The description the model is given names the shell and its version.
    // Asking for it starts the shell, which for PowerShell is most of a
    // headless run's start-up on Windows (step 13b). So the answer is kept
    // in the state directory, under the shell's path, size and time; on a
    // miss the session goes on without the version, and the shell is asked
    // on a thread of its own for the sessions after this one.
    let asks = matches!(shell.kind(), ShellKind::Bash | ShellKind::Pwsh);
    let cache = StateDir::from_dirs(&inner.user.dirs)
        .map(|state| (state.root().join(shell_cache::FILE), state));
    let stamp = shell_cache::Stamp::of(shell.program());
    match (asks, cache, stamp) {
        (true, Some((file, state)), Some(stamp)) => match shell_cache::lookup(&file, &stamp) {
            Some(version) => shell = shell.with_version(version),
            None => {
                let mut asked = shell.clone();
                let clock = Arc::clone(&inner.options.clock);
                let probe = std::thread::Builder::new()
                    .name("cortex-shell-version".to_owned())
                    .spawn(move || {
                        asked.probe_version(std::time::Duration::from_secs(2));
                        if let Some(version) = asked.version()
                            && state.ensure().is_ok()
                        {
                            shell_cache::store(&file, &stamp, version, clock.as_ref());
                        }
                    });
                match probe {
                    Ok(probe) => inner
                        .probes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(probe),
                    Err(failure) => {
                        tracing::debug!(%failure, "the shell's version is not asked for");
                    }
                }
            }
        },
        // Nowhere to keep it, a shell that cannot be looked at, or one that
        // is not asked (asking it starts nothing).
        _ => shell.probe_version(std::time::Duration::from_secs(2)),
    }
    inner
        .shells
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, shell.clone());
    Ok(shell)
}

/// The shell tool's process runner.
fn runner(inner: &Inner) -> Runner {
    if let Some(runner) = &inner.options.runner {
        return runner.clone();
    }
    let mut runner = Runner::new()
        .with_timer(Arc::clone(&inner.options.timer))
        .with_clock(Arc::clone(&inner.options.clock));
    if let Some((program, arguments)) = &inner.options.guard {
        runner = runner.with_guard(Guard::new(program.clone(), arguments.iter().cloned()));
    }
    runner
}

/// Puts a session together. Blocks.
#[expect(
    clippy::too_many_lines,
    reason = "one pass over what a session is made of, in the order it is made"
)]
pub(crate) fn assemble(
    inner: &Arc<Inner>,
    wanted: Wanted,
    cancel: CancelToken,
) -> Result<Assembled, Box<ErrorData>> {
    let options = &inner.options;
    let env = &options.env;
    let clock = Arc::clone(&options.clock);
    let mut told: Vec<String> = Vec::new();

    // ------------------------------------------------------ the workspace
    let home = env.home().as_deref().and_then(resolve_dir);
    let mut path_options = PathOptions::new();
    if let Some(home) = home {
        path_options = path_options.home(home);
    }
    let resolver = PathResolver::new(&wanted.workspace, path_options).map_err(|failure| {
        error(
            ErrorKind::Usage,
            "invalid_workspace",
            format!(
                "`{}` cannot be a workspace: {failure}",
                wanted.workspace.display()
            ),
        )
    })?;
    let resolver = Arc::new(resolver);
    let root = resolver.root().clone();
    let project = root.project_key();

    // ------------------------------------------- configuration and trust
    let mut load = LoadOptions::new(env.clone());
    if let Some(dirs) = &options.dirs {
        load.dirs = dirs.clone();
    }
    load.workspace = Some(root.to_fs_path());
    load.profile.clone_from(&options.profile);
    load.overrides.clone_from(&options.overrides);
    load.overrides.extend(wanted.overrides.iter().cloned());
    load.overrides.extend(settings_overrides(&wanted.settings));
    let state = StateDir::from_dirs(&load.dirs);
    let Trust {
        rules: rules_trusted,
        repository: repository_trusted,
        loaded,
        problem,
    } = trust(inner, state.as_ref(), &root, &load)?;
    if wanted.no_raise {
        // The level the session would start at without what the client
        // asked for, under the same answer about trust.
        let mut without = load.clone();
        without.overrides.clone_from(&options.overrides);
        without.trust = if rules_trusted {
            cortex_config::ProjectTrust::Trusted
        } else {
            cortex_config::ProjectTrust::Untrusted
        };
        let start = cortex_config::load(&without).map_err(|failure| {
            error(
                ErrorKind::Config,
                "invalid_configuration",
                failure.render(env),
            )
        })?;
        let (asked, start) = (
            &loaded.settings.permissions.mode,
            &start.settings.permissions.mode,
        );
        if !cortex_policy::level::no_higher_than(asked, start) {
            return Err(error(
                ErrorKind::Permission,
                cortex_protocol::error::code::ROLE_NOT_ALLOWED,
                format!(
                    "Only a `user` client may start a session above the level it would start \
                     at: `{}` is above `{}`.",
                    asked.as_str(),
                    start.as_str()
                ),
            ));
        }
    }
    told.extend(problem);
    for diagnostic in &loaded.diagnostics {
        told.push(diagnostic.render(env));
    }
    if !loaded.withheld.is_empty() {
        told.push(format!(
            "This project is not trusted, so {} of its settings {} not applied: {}.",
            loaded.withheld.len(),
            if loaded.withheld.len() == 1 {
                "is"
            } else {
                "are"
            },
            loaded
                .withheld
                .iter()
                .map(|withheld| format!("`{}`", withheld.key))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let loaded = Arc::new(loaded);
    let settings = &loaded.settings;

    // ---------------------------------------------------------- the models
    let late = Arc::new(LateImages::default());
    let images: Arc<dyn ImageSource> = Arc::clone(&late) as Arc<dyn ImageSource>;
    let models = models(inner, settings, &images)?;

    // -------------------------------------------- the store and the plan
    // Every key the core has found, and every one it finds from now on,
    // is masked by value in this session's log and artifacts: the filter
    // is the core's, and the session reads it each time it writes.
    let filter = Arc::clone(&inner.providers.filter);

    let (id, title, ephemeral, resume) = match &wanted.which {
        Which::New {
            id,
            title,
            ephemeral,
        } => (id.clone(), title.clone(), *ephemeral, false),
        Which::Resume { id } => (id.clone(), None, false, true),
    };
    // The harness's own git, for the base state and a file's earlier
    // content: inside the session's sandbox, once it is made, while
    // commands are confined.
    let session_git = Arc::new(SessionGit::default());
    let git: Arc<dyn Git> = options.git.clone().unwrap_or_else(|| {
        Arc::new(SystemGit::within(
            Arc::clone(&session_git) as Arc<dyn cortex_base::git::Confinement>
        ))
    });
    let mut created_at = clock.now().to_timestamp();
    let mut parent = None;
    let mut stored_title = title.clone();
    let mut lease_count = 0u64;
    let mut restored_plan: Option<Plan> = None;
    let mut base_capture = false;

    let (kept, content, rounds): (Kept, Arc<dyn ContentStore>, Option<Arc<Rounds>>) = if ephemeral {
        (Kept::memory(), Arc::new(MemoryStore::new()), None)
    } else {
        let Some(data) = loaded.data_dir() else {
            return Err(error(
                ErrorKind::Config,
                "no_data_dir",
                "There is nowhere to keep sessions: no data directory could be worked out. Set \
                 `session.data_dir`, or start the session with no log.",
            ));
        };
        let Some(sessions) = SessionsRoot::in_data_dir(&data).project(&project) else {
            return Err(error(
                ErrorKind::Internal,
                "invalid_project_key",
                "The project's key cannot be a directory name.",
            ));
        };
        let session_options = SessionOptions {
            durability: match settings.session.durability {
                cortex_config::schema::SessionDurability::Turn => Durability::Turn,
                cortex_config::schema::SessionDurability::Never => Durability::Never,
                _ => Durability::Item,
            },
            clock: Arc::clone(&clock),
            redactor: Arc::new(LogRedactor(Arc::clone(&filter))),
            harness: options.harness_version.clone(),
        };
        let session = if resume {
            let (session, report) =
                cortex_session::Session::resume(&sessions, &id, session_options)
                    .map_err(|failure| session_error(&failure))?;
            // The lease in force is read strictly: one this version does
            // not understand is a refusal, never a lease read in part.
            session.lease_in_force()?;
            lease_count = session
                .records(&cortex_session::Kind::Lease)
                .map(|records| records.len() as u64)
                .unwrap_or(0);
            created_at = session.created().clone();
            parent = session
                .header()
                .forked_from
                .as_ref()
                .map(|origin| origin.session.clone());
            stored_title = session.title().ok().flatten();
            restored_plan = session
                .read_branch()
                .ok()
                .and_then(|entries| crate::store::plan_of(&entries));
            if !report.repaired.is_empty() {
                told.push(format!(
                    "{} tool call{} of this session never finished, and {} marked as \
                     interrupted. None was run again.",
                    report.repaired.len(),
                    if report.repaired.len() == 1 { "" } else { "s" },
                    if report.repaired.len() == 1 {
                        "was"
                    } else {
                        "were"
                    },
                ));
            }
            if !report.damaged_lines.is_empty() || report.cut_tail.is_some() {
                told.push(
                    "Part of this session's log could not be read and was skipped.".to_owned(),
                );
            }
            session
        } else {
            base_capture = options.base_capture;
            cortex_session::Session::create(&sessions, id.clone(), &root, session_options)
                .map_err(|failure| session_error(&failure))?
        };
        if !session.not_private().is_empty() {
            told.push(format!(
                "The session's files are not private: other users of this machine can read {}.",
                session
                    .not_private()
                    .iter()
                    .map(|path| env.display_path(path))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let store = session.store();
        let rounds = Arc::new(Rounds::new(
            Arc::clone(&store),
            root.clone(),
            KEEP_LIMIT,
            Arc::clone(&git),
        ));
        let content: Arc<dyn ContentStore> = Arc::new(DiskContent(Arc::clone(&store)));
        (
            Kept::Disk(Disk {
                session,
                rounds: Arc::clone(&rounds),
                filter: Arc::clone(&filter),
                base: None,
            }),
            content,
            Some(rounds),
        )
    };
    let _ = late.0.set(StoredImages(Arc::clone(&content)));

    let board = Arc::new(match restored_plan {
        Some(plan) => PlanBoard::with_plan(plan),
        None => PlanBoard::new(),
    });
    let hooks = Arc::new(Hooks::new(Arc::clone(&board)));
    let sink = Arc::new(SessionSink::default());

    // ------------------------------------------------- the shell, if any
    let shell_found = match find_shell(inner, settings) {
        Ok(shell) => Some(shell),
        Err(failure) if settings.shell.program.is_some() => {
            return Err(error(
                ErrorKind::Config,
                "no_shell",
                format!("The configured shell cannot be used: {failure}"),
            ));
        }
        Err(failure) => {
            told.push(format!(
                "No shell was found, so the `shell` tool is not available: {failure}"
            ));
            None
        }
    };
    let shell = shell_found;
    // The shell as the system prompt names it.
    let shell_named = shell.as_ref().map(|shell| match shell.version() {
        Some(version) => format!("{} {version}", shell.name()),
        None => shell.name(),
    });
    let syntax = shell.as_ref().map_or(ShellSyntax::Unknown, |shell| {
        ShellSyntax::of_program(&shell.program().to_string_lossy())
    });

    // -------------------------------------------- the permission engine
    let lease_id = LeaseId::new(format!("l_{}", lease_count + 1));
    let mut parts = engine_parts(
        inner,
        &loaded,
        state.as_ref(),
        &resolver,
        &id,
        lease_id.clone(),
        rules_trusted,
        repository_trusted,
        syntax,
        &mut told,
    );
    // A write root that holds the harness's own files would let a confined
    // command change what it is allowed: the sandbox does not enforce there
    // (found by the review of step 26).
    let (mut sandbox_state, mut enforcing_with) =
        inner.sandbox.state(loaded.settings.sandbox.enabled);
    if sandbox_state == SandboxState::Enforcing
        && holds_harness_files(
            &parts.sandbox_policy,
            &loaded,
            state.as_ref(),
            inner.sandbox.program(),
        )
    {
        sandbox_state = SandboxState::NotEnforcing(NoSandbox::HarnessFilesWritable);
        enforcing_with = None;
    }
    parts.sandbox = sandbox_state;
    let template = EngineTemplate { parts };
    let engine = start_engine(template.parts.clone())?;
    let lease = engine.lease().clone();
    let sandbox_notice = engine.sandbox_notice();
    let level = engine.level().clone();

    // ------------------------------------------------------- the sandbox
    let state = sandbox_state;
    let temp_path = resolve_dir(&sandbox::temp_path(&id))
        .map_or_else(|| sandbox::temp_path(&id), |path| path.to_fs_path());
    let made = match sandbox::make_temp(&temp_path) {
        Ok(_) => true,
        Err(failure) => {
            told.push(format!(
                "The session's private temporary directory could not be made, so commands are \
                 not given one: {failure}"
            ));
            false
        }
    };
    let landlock = enforcing_with.as_ref().map(|(landlock, _)| *landlock);
    if let Some((_, gaps)) = &enforcing_with {
        told.extend(gaps.iter().map(|gap| format!("sandbox: {gap}")));
    }
    let sandbox = Arc::new(SessionSandbox::new(
        &inner.sandbox,
        state,
        landlock,
        &level,
        &lease.grant.sandbox,
        Temp {
            path: temp_path,
            made,
        },
        &root.to_fs_path(),
        options.protect_limits.unwrap_or_default(),
    ));
    session_git.set(Arc::clone(&sandbox));
    let policy = Arc::new(EnginePolicy::new(PolicyParts {
        engine,
        clients: Arc::clone(&inner.clients),
        sink: Arc::clone(&sink),
        hooks: Arc::clone(&hooks),
        timer: Arc::clone(&options.timer),
        timeout: options.approval_timeout,
        fail_on_denial: options.fail_on_denial,
        sandboxed: sandbox.confines(),
    }));

    // ------------------------------------------------------------ the tools
    let files = Arc::new(FileTable::new());
    let registry = Arc::new(ToolRegistry::new());
    let editing = match &rounds {
        Some(rounds) => Editing::new(
            Arc::clone(rounds) as Arc<dyn cortex_tools::Checkpoints>,
            Arc::clone(&clock),
        )
        .with_limit(rounds.limit()),
        // With no session on disk there is nothing to undo from: what the
        // tools changed is kept in memory, for the session's own use.
        None => Editing::new(Arc::new(MemoryCheckpoints::new()), Arc::clone(&clock))
            .with_limit(KEEP_LIMIT),
    }
    .with_sync(settings.session.durability != cortex_config::schema::SessionDurability::Never);
    let mut toolbox = Toolbox::read_only(registry)
        .with(WriteTool::new(editing.clone()))
        .with(EditTool::new(editing))
        .with(PlanTool::new(Arc::clone(&board)))
        .with(RememberTool::new(Arc::new(MemoryProposals::new())));
    if let Some(shell) = shell {
        let mut env_remove = provider::key_variables(settings);
        env_remove.extend(settings.shell.env_remove.iter().cloned());
        let config = ShellConfig {
            timeout: settings.shell.timeout,
            max_timeout: settings.shell.max_timeout,
            env_remove,
            detect_changes: true,
            // Without trust, the look for what a command changed does not
            // ask git, which the repository's own configuration could make
            // run a program.
            repository_trusted,
        };
        let mut tool = ShellTool::new(shell, config).with_runner(runner(inner));
        tool = tool
            .with_launcher(Arc::clone(&sandbox) as Arc<dyn cortex_tools::process::Launcher>)
            .with_git(Arc::clone(&session_git) as Arc<dyn cortex_base::git::Confinement>);
        if let Some(inherited) = &options.shell_env {
            tool = tool.with_inherited_env(inherited.iter().cloned());
        }
        toolbox = toolbox.with(RecordingShell::new(
            tool,
            rounds.clone(),
            Arc::clone(&sandbox),
            Arc::clone(&policy),
        ));
    }

    // ------------------------------------------- what the log starts with
    let mut kept = kept;
    if let Some(disk) = kept.disk() {
        disk.session
            .append(
                None,
                Body::Lease(LeaseRecord {
                    event: LeaseEvent::Granted,
                    lease: lease.clone(),
                }),
            )
            .map_err(|failure| session_error(&failure))?;
        if let (false, Some(title)) = (resume, &title) {
            disk.session
                .append(
                    None,
                    Body::Meta(Meta {
                        title: Some(title.clone()),
                        labels: Vec::new(),
                        note: None,
                    }),
                )
                .map_err(|failure| session_error(&failure))?;
        }
        if base_capture {
            // Taken on a thread of its own, with the answer about the
            // repository's trust, and appended when it is ready: the first
            // prompt never waits for it.
            match BaseCapture::start(
                root.clone(),
                repository_trusted,
                disk.session.store(),
                Arc::clone(&git),
                Arc::clone(&clock),
                BaseLimits::default(),
                policy.keep_gate(),
            ) {
                Ok(capture) => disk.base = Some(capture),
                Err(failure) => told.push(format!(
                    "The workspace's starting state is not being recorded: {failure}"
                )),
            }
        }
    }
    let log: SharedLog = Arc::new(std::sync::Mutex::new(kept));

    // ------------------------------------------------------------ the loop
    let main = models.main().clone();
    let workspace = Workspace {
        paths: Arc::clone(&resolver),
        content,
        files,
        limits: OutputLimits::default(),
        outliner: None,
        images: provider::image_support(&main.entry),
    };
    let info = SessionInfo {
        id: id.clone(),
        workspace: root.to_native_string(),
        title: stored_title,
        created_at,
        parent,
        level,
        lease_id: Some(lease_id),
        // Nothing is confined at `unrestricted`, wherever the sandbox
        // enforces.
        sandbox: if sandbox.confines() {
            cortex_protocol::session::SandboxStatus::Enforcing
        } else {
            cortex_protocol::session::SandboxStatus::Off
        },
    };
    let ledger = Ledger::new(Arc::clone(&clock))
        .with_account(Account::for_lease(&lease, Arc::clone(&clock)))
        .with_turn_budget(budget(&settings.limits.turn));
    // Every tool is held to a stop a confined command made.
    let names: Vec<String> = toolbox.names().map(str::to_owned).collect();
    for name in names {
        if let Some(tool) = toolbox.get(&name).cloned() {
            toolbox.insert(Arc::new(StopsWithTurn::new(tool, Arc::clone(&policy))));
        }
    }
    // What the session's context is made from. The instruction files are
    // read through the resolver and the engine's gate, and not at all when
    // `compat.instructions` is off.
    let user_dir = load.dirs.config.as_deref().and_then(resolve_dir);
    let instructions: Arc<dyn cortex_agent::InstructionFiles> = if settings.compat.instructions {
        Arc::new(DiskInstructions {
            resolver: Arc::clone(&resolver),
            user_dir: user_dir.clone(),
            gate: policy.keep_gate(),
        })
    } else {
        Arc::new(NoInstructionFiles)
    };
    let context = ContextParts {
        environment: Environment {
            os: if cfg!(windows) { "Windows" } else { "Linux" }.to_owned(),
            shell: shell_named,
            workspace: root.to_native_string(),
            git: in_git(&root),
        },
        append: inner.options.system_append.clone(),
        instructions,
        user_dir,
        settings: context_settings(settings),
        plan: Some(Arc::clone(&board)),
        cache_key: Some(id.to_string()),
    };
    let parts = SessionParts {
        info,
        models,
        speeds: speeds(settings),
        tools: Arc::new(toolbox),
        policy: Arc::clone(&policy) as Arc<dyn cortex_agent::Policy>,
        store: Box::new(LogStore::new(Arc::clone(&log), Arc::clone(&hooks))),
        context,
        events: Arc::clone(&sink) as Arc<dyn cortex_agent::EventSink>,
        stats: Arc::new(NoStats),
        verifier: Arc::new(NoVerifier),
        clock,
        cooldowns: inner.providers.cooldowns.clone(),
        ledger,
        workspace,
        config: LoopConfig::default(),
        cancel,
        observer: None,
    };

    let mut notices: Vec<Event> = Vec::new();
    if let Some(message) = sandbox_notice {
        notices.push(Event::Notice(Notice::Sandbox(SandboxNotice {
            message,
            status: status_of(state),
        })));
    }
    notices.extend(told.into_iter().map(|message| {
        Event::Notice(Notice::Info(cortex_protocol::event::InfoNotice { message }))
    }));

    Ok(Assembled {
        parts,
        sink,
        log,
        hooks,
        policy,
        board,
        sandbox,
        template,
        loaded: Arc::clone(&loaded),
        images,
        notices,
        lease_count: lease_count + 1,
    })
}
