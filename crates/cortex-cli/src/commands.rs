//! The subcommands: sessions, configuration, permissions and credentials
//! (`docs/architecture/08-configuration.md`, `06-permissions-and-sandbox.md`).
//!
//! Each one starts a core, asks it what it needs, prints and exits. What
//! they print goes to standard output; problems go to standard error. The
//! exit codes are the headless table's: 2 for a mistake or an invalid
//! configuration, 1 for anything else that went wrong.

use std::io::{self, IsTerminal, Read};
use std::sync::Arc;

use cortex_base::Secret;
use cortex_config::{Diagnostic, Env, LayerKind, LoadOptions, Platform};
use cortex_core::{ClientInfo, Core, CoreOptions, OpError, Project, TokioTimer, TrustState};
use cortex_protocol::ErrorKind;
use cortex_protocol::methods::{Op, OpResult, SessionListParams};

use crate::args::{AuthCommand, Common, ConfigCommand, PermissionsCommand, SessionsCommand};
use crate::exit;

/// Writes to standard output what a subcommand found, its control
/// characters made visible (a project's rules and settings are the
/// repository's text). A reader that has gone ends the output quietly.
macro_rules! out {
    ($($argument:tt)*) => {
        crate::print_out(&crate::visible(&format!($($argument)*)))
    };
}

/// The longest key `auth login` reads.
const KEY_LIMIT: u64 = 64 * 1024;

/// A finished subcommand: its exit code.
type Done = u8;

struct Context {
    env: Env,
    name: &'static str,
}

impl Context {
    fn new(name: &'static str) -> Self {
        Self {
            env: Env::from_process(),
            name,
        }
    }

    fn core(&self, common: &Common) -> Result<Core, Done> {
        let mut options = CoreOptions::new(self.env.clone(), Arc::new(TokioTimer));
        options.profile.clone_from(&common.profile);
        options.overrides = common
            .settings
            .iter()
            .map(|(key, value, flag)| cortex_config::Override {
                key: key.clone(),
                value: value.clone(),
                flag: flag.clone(),
            })
            .collect();
        Core::start(options).map_err(|error| self.fail(&error))
    }

    fn project(&self, core: &Core) -> Result<Project, Done> {
        let here = std::env::current_dir().map_err(|error| {
            self.say(&format!("the working directory could not be read: {error}"));
            exit::USAGE
        })?;
        core.project(&here).map_err(|error| self.fail(&error))
    }

    fn say(&self, text: &str) {
        eprintln!("{}: {}", self.name, crate::visible(text));
    }

    /// Says what went wrong and gives the exit code of its kind.
    fn fail(&self, error: &OpError) -> Done {
        self.say(&error.message);
        match error.kind {
            ErrorKind::Usage | ErrorKind::Config => exit::USAGE,
            _ => exit::INTERNAL,
        }
    }

    fn warn(&self, diagnostics: &[Diagnostic]) {
        for diagnostic in diagnostics {
            eprintln!("{}", crate::visible(&diagnostic.render(&self.env)));
        }
    }
}

fn result(outcome: Result<Done, Done>) -> Done {
    match outcome {
        Ok(code) | Err(code) => code,
    }
}

/// `cortex sessions …`
pub(crate) fn sessions(command: SessionsCommand, name: &'static str) -> Done {
    let context = Context::new(name);
    result((|| {
        let SessionsCommand::List { all, limit, json } = command;
        let core = context.core(&Common::default())?;
        let workspace = if all {
            None
        } else {
            Some(context.project(&core)?.root)
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| {
                context.say(&format!("the runtime did not start: {error}"));
                exit::INTERNAL
            })?;
        let listed = runtime.block_on(async {
            let client = core.connect(ClientInfo::observer());
            core.handle(
                client.id,
                Op::SessionList(SessionListParams {
                    workspace: workspace.map(|path| path.to_string_lossy().into_owned()),
                    limit,
                    cursor: None,
                }),
            )
            .await
        });
        let sessions = match listed {
            Ok(OpResult::SessionList(list)) => list.sessions,
            Ok(_) => return Err(exit::INTERNAL),
            Err(error) => return Err(context.fail(&error)),
        };
        let mut out = String::new();
        for session in &sessions {
            if json {
                out.push_str(&serde_json::to_string(session).unwrap_or_default());
                out.push('\n');
            } else {
                out.push_str(&format!(
                    "{}  {}  {:>3} turns  {}  {}\n",
                    session.session_id,
                    session.updated_at,
                    session.turns,
                    session.model.as_ref().map_or("-", |model| model.as_str()),
                    session.title.as_deref().unwrap_or(""),
                ));
            }
        }
        if sessions.is_empty() && !json {
            context.say(if all {
                "there are no stored sessions"
            } else {
                "there are no stored sessions in this workspace; --all lists every workspace's"
            });
        }
        out!("{out}");
        Ok(exit::COMPLETED)
    })())
}

/// `cortex models`
pub(crate) fn models(name: &'static str) -> Done {
    let context = Context::new(name);
    result((|| {
        let core = context.core(&Common::default())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| {
                context.say(&format!("the runtime did not start: {error}"));
                exit::INTERNAL
            })?;
        let listed = runtime.block_on(async {
            let client = core.connect(ClientInfo::observer());
            core.handle(
                client.id,
                Op::ModelsList(cortex_protocol::methods::Empty {}),
            )
            .await
        });
        let models = match listed {
            Ok(OpResult::ModelsList(list)) => list.models,
            Ok(_) => return Err(exit::INTERNAL),
            Err(error) => return Err(context.fail(&error)),
        };
        let mut out = String::new();
        for model in &models {
            out.push_str(&format!(
                "{}  {}  {} tokens\n",
                model.id, model.provider, model.context_window
            ));
        }
        out!("{out}");
        Ok(exit::COMPLETED)
    })())
}

/// `cortex config …`
pub(crate) fn config(command: ConfigCommand, name: &'static str) -> Done {
    let context = Context::new(name);
    result((|| match command {
        ConfigCommand::Defaults => {
            // The defaults alone: no file of the user's or the machine's.
            let mut options = LoadOptions::new(Env::empty(Platform::CURRENT));
            options.dirs.managed_config = None;
            let loaded = cortex_config::load(&options).map_err(|failure| {
                context.say(&failure.render(&context.env));
                exit::INTERNAL
            })?;
            out!("{}", loaded.show_layer(LayerKind::Default, &context.env));
            Ok(exit::COMPLETED)
        }
        ConfigCommand::Show {
            origin,
            layer,
            common,
        } => {
            let layer = match layer
                .as_deref()
                .map(|name| (name, LayerKind::from_name(name)))
            {
                Some((name, None)) => {
                    context.say(&format!(
                        "'{name}' is not a layer: default, managed, user, project, \
                         project-local, profile, environment or command-line"
                    ));
                    return Err(exit::USAGE);
                }
                Some((_, layer)) => layer,
                None => None,
            };
            let core = context.core(&common)?;
            let project = context.project(&core)?;
            let loaded = core
                .configuration(&project, false)
                .map_err(|error| context.fail(&error))?;
            context.warn(&loaded.diagnostics);
            match layer {
                Some(layer) => out!("{}", loaded.show_layer(layer, &context.env)),
                None => out!("{}", loaded.show(&context.env, origin)),
            }
            Ok(exit::COMPLETED)
        }
        ConfigCommand::Check { common } => {
            let core = context.core(&common)?;
            let project = context.project(&core)?;
            let loaded = core
                .configuration(&project, true)
                .map_err(|error| context.fail(&error))?;
            context.warn(&loaded.diagnostics);
            out!("The configuration is valid.\n");
            Ok(exit::COMPLETED)
        }
    })())
}

/// `cortex permissions …`
pub(crate) fn permissions(command: PermissionsCommand, name: &'static str) -> Done {
    let context = Context::new(name);
    result((|| {
        let core = context.core(&Common::default())?;
        let project = context.project(&core)?;
        let fail = |error: OpError| context.fail(&error);
        match command {
            PermissionsCommand::List => {
                let trust = core.project_trust(&project).map_err(fail)?;
                let approvals = core.stored_approvals(&project).map_err(fail)?;
                let mut out = format!(
                    "{}: {}\n",
                    project.shown,
                    match trust {
                        TrustState::Untrusted => "not trusted",
                        TrustState::Trusted => "trusted",
                        TrustState::RulesChanged =>
                            "trusted, but its permission rules have changed since; approve them \
                             with `permissions trust`",
                    }
                );
                if approvals.is_empty() {
                    out.push_str("No stored approvals.\n");
                } else {
                    out.push_str("Stored approvals:\n");
                    for approval in &approvals {
                        out.push_str(&format!(
                            "  {}  {} {} {}  (since {})\n",
                            approval.id,
                            approval.rule.action.as_str(),
                            approval.rule.effect.as_str(),
                            approval.rule.pattern,
                            approval.created_at,
                        ));
                    }
                }
                out!("{out}");
                Ok(exit::COMPLETED)
            }
            PermissionsCommand::Revoke { ids } => {
                let ids: Vec<_> = ids.iter().map(|id| id.as_str().into()).collect();
                let removed = core.revoke_approvals(&project, &ids).map_err(fail)?;
                if removed == 0 {
                    context.say("no stored approval of this project has those ids");
                    return Err(exit::USAGE);
                }
                out!("Removed {removed} of {}.\n", ids.len());
                Ok(exit::COMPLETED)
            }
            PermissionsCommand::Trust => {
                let trusted = core.trust_project(&project).map_err(fail)?;
                let mut out = format!(
                    "{} {}.\n",
                    if trusted.was_trusted {
                        "Approved the permission rules of"
                    } else {
                        "Trusted"
                    },
                    project.shown
                );
                if !trusted.rules.is_empty() {
                    out.push_str("Its permission rules, as they are now:\n");
                    for rule in &trusted.rules {
                        out.push_str(&format!(
                            "  {} {} {}\n",
                            rule.action.as_str(),
                            rule.effect.as_str(),
                            rule.pattern
                        ));
                    }
                }
                if !trusted.settings.is_empty() {
                    out.push_str("Its settings that now apply:\n");
                    for setting in &trusted.settings {
                        out.push_str(&format!("  {} = {}\n", setting.key, setting.value));
                    }
                }
                out!("{out}");
                Ok(exit::COMPLETED)
            }
            PermissionsCommand::Untrust => {
                if core.untrust_project(&project).map_err(fail)? {
                    out!("{} is not trusted any more.\n", project.shown);
                } else {
                    out!("{} was not trusted.\n", project.shown);
                }
                Ok(exit::COMPLETED)
            }
        }
    })())
}

/// `cortex auth …`
pub(crate) fn auth(command: AuthCommand, name: &'static str) -> Done {
    let context = Context::new(name);
    result((|| {
        let core = context.core(&Common::default())?;
        match command {
            AuthCommand::Status => {
                let mut out = String::new();
                for provider in core.credentials() {
                    let line = match &provider.status {
                        Ok(status) => {
                            let mut line = match &status.active {
                                Some(source) => format!("key from {source}"),
                                None => "no key".to_owned(),
                            };
                            if status.stored_but_unused {
                                line.push_str("; a stored key is not used");
                            }
                            if let Some(warning) = &status.store_warning {
                                line.push_str(&format!("; {warning}"));
                            }
                            line
                        }
                        Err(error) => format!("unknown: {error}"),
                    };
                    out.push_str(&format!("{}: {line}\n", provider.provider));
                }
                out!("{out}");
                Ok(exit::COMPLETED)
            }
            AuthCommand::Login { provider } => {
                known_provider(&context, &core, &provider)?;
                let stdin = io::stdin();
                if stdin.is_terminal() {
                    context.say(&format!(
                        "give the key on standard input, so that it is not echoed or kept in \
                         the shell's history: `{} auth login {provider} < key-file`",
                        context.name
                    ));
                    return Err(exit::USAGE);
                }
                let mut key = String::new();
                stdin
                    .lock()
                    .take(KEY_LIMIT + 1)
                    .read_to_string(&mut key)
                    .map_err(|error| {
                        context.say(&format!("standard input could not be read: {error}"));
                        exit::USAGE
                    })?;
                if key.len() as u64 > KEY_LIMIT {
                    context.say("standard input holds more than a key: 64 KiB at most");
                    return Err(exit::USAGE);
                }
                let key = key.trim();
                if key.is_empty() {
                    context.say("standard input held no key");
                    return Err(exit::USAGE);
                }
                core.store_credential(&provider, &Secret::new(key))
                    .map_err(|error| {
                        context.say(&error.message);
                        exit::AUTH
                    })?;
                out!("Stored the key of {provider}.\n");
                Ok(exit::COMPLETED)
            }
            AuthCommand::Logout { provider } => {
                known_provider(&context, &core, &provider)?;
                let removed = core.remove_credential(&provider).map_err(|error| {
                    context.say(&error.message);
                    exit::AUTH
                })?;
                if removed {
                    out!("Removed the stored key of {provider}.\n");
                } else {
                    out!("No key of {provider} was stored.\n");
                }
                Ok(exit::COMPLETED)
            }
        }
    })())
}

/// Refuses a provider the core does not know: a key stored under a typo
/// would be kept and never used.
fn known_provider(context: &Context, core: &Core, provider: &str) -> Result<(), Done> {
    let known: Vec<String> = core
        .credentials()
        .into_iter()
        .map(|credential| credential.provider)
        .collect();
    if known.iter().any(|name| name == provider) {
        return Ok(());
    }
    context.say(&format!(
        "there is no provider `{provider}`. The providers are: {}",
        known.join(", ")
    ));
    Err(exit::USAGE)
}
