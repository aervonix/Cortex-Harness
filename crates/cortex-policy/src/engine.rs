//! The permission engine: one decision for one planned tool call
//! (`docs/architecture/06-permissions-and-sandbox.md`).
//!
//! An [`Engine`] holds a session's standing lease and the rules in force,
//! and answers [`Engine::decide`] for a [`Request`]: what a planned call
//! would do, as a list of [`Access`]es. The answer is to allow it, to ask,
//! or to deny it ([`Outcome`]).
//!
//! # How a call is decided
//!
//! Each access is judged, and the call gets the strictest answer of them:
//! a deny beats an ask beats an allow.
//!
//! 1. **A level that refuses is final.** At `read-only`, a write does not
//!    happen whatever a rule allows.
//! 2. **Rules settle what they match**: `deny`, then `ask`, then `allow`.
//! 3. **Otherwise the level decides** who does: the holder (it runs) or
//!    the supervisor (it is asked about). What counts as inside the lease's
//!    scope is its writable roots, less its protected paths. A file that
//!    looks like a secret is beyond the scope wherever it is.
//!
//! # Commands
//!
//! A command line is analysed ([`crate::command`]), and then:
//!
//! - **A `deny` or `ask` rule is matched first**, against the raw line and
//!   every analysed part.
//! - **A line the analysis does not model** is never on a safe list and no
//!   allow rule applies to it. The level decides it as any other command:
//!   asked about at `supervised`, run at `autonomous`, where the sandbox
//!   confines it.
//! - **Each part of a modelled line must be allowed**: by the table of
//!   read-only commands, or by an allow rule. A line is on a safe list
//!   only if every part is.
//! - **What a command names is checked as a read.** `cat` only reads, and
//!   `cat ~/.ssh/id_rsa` reads a private key: every file a table command
//!   names, and the directory it runs in, is put to the same judgement a
//!   `read` call gets, and the command is safe only if each would be
//!   allowed unasked. For every other modelled command, an argument that
//!   names a path a rule denies or asks about, or the harness's own files,
//!   makes the command one to ask about.
//! - **`cd dir && …` is followed.** The parts after it are judged from the
//!   new directory. A `cd` whose effect cannot be known (in a pipeline,
//!   before `;` or `||`) takes the line off every safe list.
//! - **A redirection is a write** of its file, judged as one. `/dev/null`
//!   is nothing.
//! - **A line whose every part only reads is marked read-only**
//!   ([`Outcome::Allow`]), which is how the loop comes to run it beside
//!   other calls.
//!
//! The `write` access a shell call declares for the workspace root says "a
//! command may change anything here". It is answered by the decision about
//! the command, and is not judged a second time as an edit. A `write` of a
//! directory outside the scope (a command run from elsewhere) asks.
//!
//! # What this module does not do
//!
//! It asks nobody. Putting a [`Question`] to a person, timing the wait and
//! turning "nobody can be asked" into a denial or the end of the turn are
//! the core's ([`Question::refusal`] is the text for that case). The engine
//! records the answer ([`Engine::record`]).

use std::sync::{Arc, PoisonError, RwLock};

use cortex_base::{AbsPath, PathResolver, PathStyle, SharedClock};
use cortex_protocol::ids::{ApprovalId, LeaseId, ModelId};
use cortex_protocol::lease::{
    Budget, Effect, HardLimits, Lease, Level, Rule, RuleAction, SandboxPolicy,
};
use cortex_protocol::methods::{ApprovalChoice, StoredApproval};

use crate::approvals::{ApprovalError, ApprovalStore, SessionApprovals};
use crate::command::pattern::words_of;
use crate::command::{Analysis, Joiner, Part, Redirect, ShellSyntax, analyse, table};
use crate::lease::{StandingTerms, standing_lease};
use crate::level::{self, Decider, LevelError, Matter, SandboxState};
use crate::rules::{By, Hit, Protected, RuleContext, RuleError, RuleSet, RuleSource, SourcedRule};
use crate::secrets::looks_secret;

/// One thing a planned call would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Reads a file, or the names in a directory. `path` is resolved.
    Read {
        /// The real location.
        path: AbsPath,
    },
    /// Walks the tree under a directory. `path` is resolved. Each entry
    /// the walk meets is then put to [`Engine::may_read`].
    ReadTree {
        /// The real location of the directory.
        path: AbsPath,
    },
    /// Creates or changes a file. `path` is resolved.
    Write {
        /// The real location.
        path: AbsPath,
    },
    /// Runs a command.
    Exec {
        /// The command line, exactly as the model wrote it.
        command: String,
        /// The directory it would run in, resolved.
        cwd: AbsPath,
    },
    /// Reaches the network.
    Network {
        /// The host, or `None` for a call that cannot say.
        host: Option<String>,
    },
    /// Calls an extension or MCP tool.
    Ext {
        /// The tool's full name, `source/tool`.
        tool: String,
    },
}

/// A planned call, as the engine is shown it.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The tool's name, for explanations.
    pub tool: &'a str,
    /// What the tool's own spec says of a call no rule and no level
    /// decides: `allow` for the built-in tools, `ask` for tools from
    /// extensions and MCP servers.
    pub default: RuleAction,
    /// Everything the call would do.
    pub accesses: &'a [Access],
}

/// What the engine says of a planned call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It may run.
    Allow {
        /// Whether the call is a command known to change nothing, to be
        /// scheduled as read-only.
        read_only: bool,
    },
    /// Whoever supervises the session decides.
    Ask(Question),
    /// It may not run.
    Deny {
        /// Why, for the model.
        reason: String,
    },
}

/// Why a call is asked about, and what an approval could add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// Each reason the call needs an answer, in a sentence.
    pub reasons: Vec<String>,
    /// The allow rules that "allow for this session" or "always allow in
    /// this project" would add, shown to the person before they choose.
    /// Empty when no rule could express the approval: then only "allow
    /// once" and "deny" are on offer.
    pub lifts: Vec<Rule>,
}

impl Question {
    /// The answers on offer.
    #[must_use]
    pub fn choices(&self) -> Vec<ApprovalChoice> {
        let mut choices = vec![ApprovalChoice::AllowOnce];
        if !self.lifts.is_empty() {
            choices.push(ApprovalChoice::AllowSession);
            choices.push(ApprovalChoice::AllowProject);
        }
        choices.push(ApprovalChoice::Deny);
        choices
    }

    /// What the model is told when the call needs an answer and nobody can
    /// be asked: a front end without a person, a run that cannot ask.
    #[must_use]
    pub fn refusal(&self) -> String {
        format!(
            "This call needs approval, and nobody can be asked: {} Allow it beforehand with a \
             permission rule or a permission mode, or do the task another way.",
            self.reasons.join(" ")
        )
    }
}

/// What recording an answer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// Whether the call may run.
    pub allowed: bool,
    /// The approvals that were added, for the session or for the project.
    pub added: Vec<StoredApproval>,
}

/// Finds where a path a command names leads.
///
/// The file system is asked, because a link in the workspace can lead
/// anywhere. The engine never runs anything to find out: it looks up
/// names. A test stands in with [`Lexical`].
pub trait Locate: Send + Sync + std::fmt::Debug {
    /// The real location of `written`, as a command running in `cwd` would
    /// reach it: relative to `cwd`, absolute, or under `~`. `None` when it
    /// is not a path the harness accepts, or cannot be followed.
    fn locate(&self, cwd: &AbsPath, written: &str) -> Option<AbsPath>;
}

/// Whether a path as written starts somewhere other than the directory
/// a command runs in: at a root, at a drive, at a share, or at something
/// only the resolver can refuse (`\\Windows` and `C:file` on Windows, which
/// depend on a current drive nobody stated). Such a path is never joined to
/// the working directory: joined, `\\Windows\\win.ini` would look like a file
/// in the workspace.
fn starts_elsewhere(written: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => written.starts_with('/'),
        PathStyle::Windows => {
            written.starts_with(['/', '\\']) || written.chars().nth(1) == Some(':')
        }
    }
}

impl Locate for PathResolver {
    fn locate(&self, cwd: &AbsPath, written: &str) -> Option<AbsPath> {
        let is_home = written == "~" || written.starts_with("~/") || written.starts_with("~\\");
        let resolved = if is_home || starts_elsewhere(written, PathStyle::NATIVE) {
            self.resolve(written)
        } else {
            let separator = if cfg!(windows) { "\\" } else { "/" };
            self.resolve(&format!("{}{separator}{written}", cwd.to_native_string()))
        };
        resolved.ok().map(|resolved| resolved.abs().clone())
    }
}

/// A [`Locate`] that reads paths as text and asks no file system: links
/// are not followed. For tests, and for nothing that grants.
#[derive(Debug, Clone, Default)]
pub struct Lexical {
    /// The home directory, for `~`.
    pub home: Option<AbsPath>,
}

impl Locate for Lexical {
    fn locate(&self, cwd: &AbsPath, written: &str) -> Option<AbsPath> {
        if written == "~" {
            return self.home.clone();
        }
        let under_home = written.strip_prefix("~/").or_else(|| {
            // Windows takes either separator.
            (cwd.style() == PathStyle::Windows)
                .then(|| written.strip_prefix("~\\"))
                .flatten()
        });
        if let Some(rest) = under_home {
            return self.home.as_ref()?.join(rest).ok();
        }
        if starts_elsewhere(written, cwd.style()) {
            return AbsPath::parse(written, cwd.style()).ok();
        }
        // `..` is taken as text: enough for a test, and not how the file
        // system does it through a link.
        AbsPath::parse(&format!("{}/{written}", cwd.as_str()), cwd.style()).ok()
    }
}

/// Everything an engine is made from.
#[derive(Debug, Clone)]
pub struct Parts {
    /// The id of the session's standing lease.
    pub lease_id: LeaseId,
    /// The permission mode.
    pub level: Level,
    /// The rules of configuration, flags and the project's learned
    /// commands, each with who wrote it.
    pub rules: Vec<SourcedRule>,
    /// The "always allow in this project" rules, as loaded.
    pub approved: Vec<StoredApproval>,
    /// What a command may touch: the scope of the lease.
    pub sandbox_policy: SandboxPolicy,
    /// Whether the sandbox confines commands.
    pub sandbox: SandboxState,
    /// The models the lease names. Empty: the configured ones.
    pub models: Vec<ModelId>,
    /// The session's limits.
    pub budget: Budget,
    /// What no level relaxes.
    pub limits: HardLimits,
    /// The workspace root, resolved.
    pub workspace: AbsPath,
    /// The home directory.
    pub home: Option<AbsPath>,
    /// Whether the user has trusted the project, and its allow rules are
    /// the ones they approved ([`crate::trust::ProjectTrust::allows_rules`]).
    pub trusted: bool,
    /// The harness's own files.
    pub protected: Protected,
    /// The language the shell tool's commands are written in.
    pub shell: ShellSyntax,
    /// How a path a command names is followed.
    pub locate: Arc<dyn Locate>,
    /// Where "always allow in this project" is stored. `None`: that choice
    /// is not on offer.
    pub store: Option<ApprovalStore>,
    /// The clock, for the time an approval is given.
    pub clock: SharedClock,
}

/// Why an engine could not be made.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The level cannot be held here.
    #[error(transparent)]
    Level(#[from] LevelError),
    /// A rule cannot be used. Nothing is dropped: the session does not
    /// start with a rule missing.
    #[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Rules(Vec<RuleError>),
    /// A writable root or a protected path is not a path. A protected path
    /// that was skipped would not be protected, so the session does not
    /// start.
    #[error(
        "`{entry}` in `{list}` is not a path Cortex-Harness accepts. Write an absolute path, one \
         that starts with `~/`, or one relative to the workspace"
    )]
    Sandbox {
        /// The entry, as written.
        entry: String,
        /// The setting it is in.
        list: &'static str,
    },
}

/// The rules in force and where they came from, behind the engine's lock.
#[derive(Debug)]
struct Rules {
    /// Configuration, flags, learned commands.
    configured: Vec<SourcedRule>,
    /// Stored for the project.
    approved: Vec<StoredApproval>,
    /// Held for the session.
    session: SessionApprovals,
    compiled: RuleSet,
}

/// The permission engine of one session.
#[derive(Debug)]
pub struct Engine {
    lease: Lease,
    workspace: AbsPath,
    home: Option<AbsPath>,
    trusted: bool,
    protected: Protected,
    shell: ShellSyntax,
    sandbox: SandboxState,
    locate: Arc<dyn Locate>,
    store: Option<ApprovalStore>,
    clock: SharedClock,
    /// The roots of the lease's scope, read once.
    roots: Vec<AbsPath>,
    /// The protected paths, read once.
    guarded: Vec<AbsPath>,
    rules: RwLock<Rules>,
}

/// What one access comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Finding {
    Allow,
    Ask {
        reason: String,
        /// The allow rule that would settle it next time, if one could.
        lift: Option<Rule>,
    },
    Deny {
        reason: String,
        /// Whether the denial is the built-in protection of the harness's
        /// own files, which stays a denial even for a path a command only
        /// mentions.
        built_in: bool,
    },
}

impl Finding {
    fn ask(reason: impl Into<String>) -> Self {
        Self::Ask {
            reason: reason.into(),
            lift: None,
        }
    }

    fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
            built_in: false,
        }
    }
}

/// How a part of a command line is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// By the table of commands that only read.
    ReadOnly,
    /// By an allow rule, or by the table with a redirection that writes.
    Safe,
    /// By nothing.
    Other,
}

impl Engine {
    /// Makes the engine of a session, and with it the session's standing
    /// lease.
    ///
    /// # Errors
    ///
    /// When the level cannot be held with the sandbox as it is, or a rule
    /// cannot be compiled.
    pub fn start(mut parts: Parts) -> Result<Self, StartError> {
        // The scope and the protected paths, as absolute paths. The lease
        // then holds them in that form, so that it means the same wherever
        // it is read.
        let roots = place_all(
            &mut parts.sandbox_policy.write.roots,
            "sandbox.write",
            &parts.workspace,
            parts.home.as_ref(),
        )?;
        let guarded = place_all(
            &mut parts.sandbox_policy.protect.roots,
            "sandbox.protect",
            &parts.workspace,
            parts.home.as_ref(),
        )?;
        let compiled = compile(
            &parts.rules,
            &parts.approved,
            &SessionApprovals::new(),
            &parts.workspace,
            parts.home.as_ref(),
            parts.trusted,
            &parts.protected,
        )
        .map_err(StartError::Rules)?;
        let lease = standing_lease(
            parts.lease_id,
            StandingTerms {
                level: parts.level,
                rules: compiled.rules().map(|rule| rule.rule.clone()).collect(),
                sandbox: parts.sandbox_policy,
                models: parts.models,
                budget: parts.budget,
                limits: parts.limits,
            },
            parts.sandbox,
        )?;
        Ok(Self {
            roots,
            guarded,
            lease,
            workspace: parts.workspace,
            home: parts.home,
            trusted: parts.trusted,
            protected: parts.protected,
            shell: parts.shell,
            sandbox: parts.sandbox,
            locate: parts.locate,
            store: parts.store,
            clock: parts.clock,
            rules: RwLock::new(Rules {
                configured: parts.rules,
                approved: parts.approved,
                session: SessionApprovals::new(),
                compiled,
            }),
        })
    }

    /// The session's standing lease, as it was when the session started.
    /// Its rules are the rules that were in force then: approvals given
    /// since are in [`approvals`](Self::approvals).
    #[must_use]
    pub fn lease(&self) -> &Lease {
        &self.lease
    }

    /// The level.
    #[must_use]
    pub fn level(&self) -> &Level {
        &self.lease.grant.level
    }

    /// One line to show when the sandbox does not confine commands, and
    /// `None` when it does.
    #[must_use]
    pub fn sandbox_notice(&self) -> Option<String> {
        level::sandbox_notice(self.level(), self.sandbox)
    }

    /// The project's allow rules that are set aside because the project is
    /// not trusted.
    #[must_use]
    pub fn withheld(&self) -> Vec<SourcedRule> {
        self.read_rules().compiled.withheld().to_vec()
    }

    /// The approvals in force: stored for the project, then given for the
    /// session.
    #[must_use]
    pub fn approvals(&self) -> Vec<StoredApproval> {
        let rules = self.read_rules();
        rules
            .approved
            .iter()
            .chain(rules.session.list())
            .cloned()
            .collect()
    }

    fn read_rules(&self) -> std::sync::RwLockReadGuard<'_, Rules> {
        self.rules.read().unwrap_or_else(PoisonError::into_inner)
    }

    // ------------------------------------------------------------ deciding

    /// Decides a planned call. Asks nobody.
    #[must_use]
    pub fn decide(&self, request: &Request<'_>) -> Outcome {
        let rules = self.read_rules();
        let rules = &rules.compiled;
        let command = request
            .accesses
            .iter()
            .any(|access| matches!(access, Access::Exec { .. }));
        let mut findings = Vec::new();
        let mut read_only = command;
        for access in request.accesses {
            match access {
                Access::Read { path } | Access::ReadTree { path } => {
                    findings.push(self.judge_path(rules, &Effect::Read, path, None));
                }
                Access::Write { path } if !command => {
                    findings.push(self.judge_path(rules, &Effect::Write, path, None));
                }
                // Judged with the command, below.
                Access::Write { .. } => {}
                Access::Exec { command, cwd } => {
                    let (found, only_reads) = self.judge_command(rules, command, cwd);
                    read_only &= only_reads;
                    findings.extend(found);
                }
                Access::Network { host } => {
                    findings.push(self.judge_network(rules, host.as_deref()));
                }
                Access::Ext { tool } => {
                    findings.push(self.judge_ext(rules, tool, request.default));
                }
            }
        }
        // What a command might write, declared beside it: nothing for a
        // command that only reads.
        if command && !read_only {
            for access in request.accesses {
                if let Access::Write { path } = access {
                    findings.push(self.judge_command_write(rules, path));
                }
            }
        }
        // A tool's own default is for a call that no rule and no level
        // decides: one that declares nothing at all. (For an extension's
        // tool it was used above, with the rules on the tool's name.)
        if request.accesses.is_empty() {
            match request.default {
                RuleAction::Allow => {}
                // At `unrestricted` nothing is asked. A tool that is turned
                // off stays off.
                RuleAction::Ask if *self.level() == Level::Unrestricted => {}
                RuleAction::Ask => findings.push(Finding::ask(format!(
                    "The tool `{}` asks before every call.",
                    request.tool
                ))),
                RuleAction::Deny => findings.push(Finding::deny(format!(
                    "The tool `{}` is turned off.",
                    request.tool
                ))),
            }
        }
        combine(findings, read_only)
    }

    /// Whether a directory walk may read the entry at `path`: what
    /// `cortex-tools`' `ReadGate` asks.
    ///
    /// The walk's tree was decided as a whole before it began, so the
    /// scope is not asked again here. What is asked is what a tree cannot
    /// say in advance: whether a rule protects this entry, and whether it
    /// looks like a secret. An entry that would have to be asked about is
    /// stepped past: a walk asks nobody.
    #[must_use]
    pub fn may_read(&self, path: &AbsPath) -> bool {
        let rules = self.read_rules();
        match rules.compiled.path(&Effect::Read, path) {
            Some(hit) => hit.action == RuleAction::Allow,
            None => {
                let matter = if self.is_secret(path) {
                    Matter::ReadBeyond
                } else {
                    Matter::Read
                };
                level::decider(self.level(), matter) == Decider::Holder
            }
        }
    }

    /// Whether the content of the file at `path` may be kept in a
    /// session's base state: what `cortex-session`'s `KeepGate` asks.
    ///
    /// A file is kept when the read rules would let the agent read it
    /// without asking, and the level is not part of the answer: nobody
    /// approved the base state, so not even `unrestricted` puts a file that
    /// looks like a secret into the checkpoint store. A rule that allows
    /// reading the file does.
    #[must_use]
    pub fn may_store(&self, path: &AbsPath) -> bool {
        let rules = self.read_rules();
        match rules.compiled.path(&Effect::Read, path) {
            Some(hit) => hit.action == RuleAction::Allow,
            None => self.in_scope(path) && !self.is_secret(path),
        }
    }

    fn is_secret(&self, path: &AbsPath) -> bool {
        path.file_name().is_some_and(looks_secret)
    }

    /// Whether `path` is inside the lease's scope: under one of its
    /// writable roots, with names compared exactly.
    fn in_scope(&self, path: &AbsPath) -> bool {
        self.lease.grant.sandbox.write.all
            || self.roots.iter().any(|root| path.starts_with_exactly(root))
    }

    /// Whether `path` is one of the lease's protected paths, or inside
    /// one, or part of a git repository's own data. Compared as the style
    /// compares names: protecting too much is the safe side.
    fn is_guarded(&self, path: &AbsPath) -> bool {
        self.lease.grant.sandbox.protect.all
            || self.guarded.iter().any(|guard| path.starts_with(guard))
            || is_repository_data(path)
    }

    /// Whether writing `path` always goes to the supervisor, short of
    /// `unrestricted`: a protected path, or a file that looks like a
    /// secret, that no allow rule names.
    fn is_sensitive(&self, rules: &RuleSet, path: &AbsPath) -> bool {
        (self.is_guarded(path) || self.is_secret(path))
            && level::decider(self.level(), Matter::WriteBeyond) != Decider::Holder
            && !matches!(
                rules.path(&Effect::Write, path),
                Some(hit) if hit.action == RuleAction::Allow
            )
    }

    /// Judges a read or a write of a path. `named_by` is the command that
    /// names the path, when one does, for the wording.
    fn judge_path(
        &self,
        rules: &RuleSet,
        effect: &Effect,
        path: &AbsPath,
        named_by: Option<&str>,
    ) -> Finding {
        let writes = *effect == Effect::Write;
        let secret = self.is_secret(path);
        let beyond = if !self.in_scope(path) {
            Some("is outside the workspace")
        } else if secret {
            Some("looks like a file that holds secrets")
        } else if writes && self.is_guarded(path) {
            Some("is a protected path: whoever can write it can run code later")
        } else {
            None
        };
        let matter = match (writes, beyond.is_some()) {
            (false, false) => Matter::Read,
            (false, true) => Matter::ReadBeyond,
            (true, false) => Matter::Edit,
            (true, true) => Matter::WriteBeyond,
        };
        let what = match named_by {
            Some(command) => format!("`{command}` names `{path}`, which"),
            None => format!("`{path}`"),
        };
        let decider = level::decider(self.level(), matter);
        if decider == Decider::Refused {
            return Finding::deny(format!(
                "The session is at the level `{}`, which does not {} {}.",
                self.level().as_str(),
                if writes { "write" } else { "read" },
                match named_by {
                    Some(_) => format!("`{path}`"),
                    None => "files".to_owned(),
                }
            ));
        }
        match rules.path(effect, path) {
            Some(hit) if hit.action == RuleAction::Deny => Finding::Deny {
                reason: format!("{what} is protected by {}.", hit.describe()),
                built_in: matches!(hit.by, By::Builtin(_)),
            },
            Some(hit) if hit.action == RuleAction::Ask => {
                Finding::ask(format!("{what} is asked about by {}.", hit.describe()))
            }
            Some(_) => Finding::Allow,
            None if decider == Decider::Holder => Finding::Allow,
            None => Finding::Ask {
                reason: format!(
                    "{what} {}.",
                    beyond.unwrap_or("needs the supervisor's answer")
                ),
                lift: Some(Rule {
                    effect: effect.clone(),
                    pattern: literal_pattern(path),
                    action: RuleAction::Allow,
                }),
            },
        }
    }

    /// Judges the write a shell call declares beside its command: "the
    /// command may change anything here". Inside the scope that is the
    /// command's own question. Outside it, or where a rule holds writes
    /// back, it is one more.
    fn judge_command_write(&self, rules: &RuleSet, path: &AbsPath) -> Finding {
        match rules.path(&Effect::Write, path) {
            Some(hit) if hit.action == RuleAction::Deny => {
                return Finding::Deny {
                    reason: format!(
                        "A command may write anywhere in `{path}`, which is protected by {}.",
                        hit.describe()
                    ),
                    built_in: matches!(hit.by, By::Builtin(_)),
                };
            }
            Some(hit) if hit.action == RuleAction::Ask => {
                return Finding::ask(format!(
                    "A command may write anywhere in `{path}`, which is asked about by {}.",
                    hit.describe()
                ));
            }
            Some(_) => return Finding::Allow,
            None => {}
        }
        if *self.level() == Level::Unrestricted {
            Finding::Allow
        } else if !self.in_scope(path) {
            Finding::ask(format!(
                "The command would run in `{path}`, outside the workspace, and may write there."
            ))
        } else if self.is_sensitive(rules, path) {
            // A tool that declares a write of one file beside its command.
            Finding::ask(format!(
                "The call would write `{path}`, which is a protected path or looks like a file \
                 that holds secrets."
            ))
        } else {
            Finding::Allow
        }
    }

    fn judge_network(&self, rules: &RuleSet, host: Option<&str>) -> Finding {
        let shown = host.unwrap_or("any host");
        match rules.network(host) {
            Some(hit) if hit.action == RuleAction::Deny => Finding::deny(format!(
                "Reaching {shown} is refused by {}.",
                hit.describe()
            )),
            Some(hit) if hit.action == RuleAction::Ask => Finding::ask(format!(
                "Reaching {shown} is asked about by {}.",
                hit.describe()
            )),
            Some(_) => Finding::Allow,
            None if level::decider(self.level(), Matter::Network) == Decider::Holder => {
                Finding::Allow
            }
            None => Finding::Ask {
                reason: format!("The call reaches the network ({shown})."),
                lift: host.map(|host| Rule {
                    effect: Effect::Network,
                    pattern: host.trim_end_matches('.').to_ascii_lowercase(),
                    action: RuleAction::Allow,
                }),
            },
        }
    }

    fn judge_ext(&self, rules: &RuleSet, tool: &str, default: RuleAction) -> Finding {
        let action = match rules.ext(tool) {
            Some(hit) => {
                return match hit.action {
                    RuleAction::Allow => Finding::Allow,
                    RuleAction::Ask => Finding::ask(format!(
                        "The tool `{tool}` is asked about by {}.",
                        hit.describe()
                    )),
                    RuleAction::Deny => Finding::deny(format!(
                        "The tool `{tool}` is refused by {}.",
                        hit.describe()
                    )),
                };
            }
            None if *self.level() == Level::Unrestricted && default != RuleAction::Deny => {
                RuleAction::Allow
            }
            None => default,
        };
        match action {
            RuleAction::Allow => Finding::Allow,
            RuleAction::Deny => Finding::deny(format!("The tool `{tool}` is turned off.")),
            RuleAction::Ask => Finding::Ask {
                reason: format!(
                    "The tool `{tool}` comes from an extension or an MCP server, and asks before \
                     it is used."
                ),
                lift: Some(Rule {
                    effect: Effect::Ext,
                    pattern: literal_name(tool),
                    action: RuleAction::Allow,
                }),
            },
        }
    }

    /// Judges a command line. The findings, and whether every part of the
    /// line is a command that only reads.
    fn judge_command(&self, rules: &RuleSet, command: &str, cwd: &AbsPath) -> (Vec<Finding>, bool) {
        let analysis = analyse(command, self.shell);
        let mut findings = Vec::new();
        match rules.exec_restriction(command, analysis.parts(), self.shell) {
            Some(hit) if hit.action == RuleAction::Deny => {
                let reason = format!("The command is refused by {}.", hit.describe());
                return (vec![Finding::deny(reason)], false);
            }
            Some(hit) => findings.push(Finding::ask(format!(
                "The command is asked about by {}.",
                hit.describe()
            ))),
            None => {}
        }
        let parts = match &analysis {
            Analysis::Parts(parts) => parts,
            Analysis::NotModelled { why } => {
                findings.push(self.other_command(format!(
                    "The command has {why}. The analysis does not model that, so no rule and no \
                     safe list applies to it."
                )));
                return (findings, false);
            }
        };

        let mut directory = Some(cwd.clone());
        let mut only_reads = true;
        for (index, part) in parts.iter().enumerate() {
            let Some(here) = directory.clone() else {
                // A directory change the analysis could not follow: from
                // here on, nothing can be said of what a part reads.
                findings.push(self.other_command(
                    "The command changes directory in a way the analysis cannot follow, so what \
                     comes after it cannot be checked."
                        .to_owned(),
                ));
                return (findings, false);
            };
            let changes_directory = is_change_of_directory(part, self.shell);
            let later = parts.get(index + 1..).unwrap_or(&[]);
            if changes_directory && part.redirects.is_empty() {
                // A last `cd` changes nothing that is checked. Any other is
                // followed where its effect is certain, and where it is
                // not, nothing after it can be judged.
                directory = if later.is_empty() {
                    Some(here)
                } else {
                    self.follow_cd(part, later, &here)
                };
                continue;
            }
            if changes_directory && !later.is_empty() {
                // A `cd` with a redirection: judged as any command, for
                // what it writes, and not followed.
                directory = None;
            }
            let (standing, found) = self.judge_part(rules, part, &here);
            findings.extend(found);
            only_reads &= standing == Standing::ReadOnly;
            let matter = match standing {
                Standing::ReadOnly => Matter::ReadOnlyCommand,
                Standing::Safe => Matter::SafeCommand,
                Standing::Other => Matter::OtherCommand,
            };
            if level::decider(self.level(), matter) != Decider::Holder {
                // An approval can add an allow rule only where an allowed
                // command would then run unasked.
                let lift = (standing == Standing::Other
                    && level::decider(self.level(), Matter::SafeCommand) == Decider::Holder)
                    .then(|| approval_rule(part))
                    .flatten();
                let reason = match standing {
                    Standing::Other => {
                        format!("`{}` is not on a safe list.", words_of(part).join(" "))
                    }
                    _ => format!(
                        "`{}` may change files, and the level `{}` asks about every command that \
                         is not known to only read.",
                        words_of(part).join(" "),
                        self.level().as_str()
                    ),
                };
                findings.push(Finding::Ask { reason, lift });
            }
        }
        (findings, only_reads)
    }

    /// The finding for a command the level decides as "any other command".
    fn other_command(&self, reason: String) -> Finding {
        if level::decider(self.level(), Matter::OtherCommand) == Decider::Holder {
            Finding::Allow
        } else {
            Finding::ask(reason)
        }
    }

    /// Where a `cd` leads, when that is certain for every part after it.
    ///
    /// - It has one plain argument.
    /// - It is not in a pipeline, where it runs in a shell of its own, and
    ///   it does not follow `||`, where it runs only if what came before
    ///   failed.
    /// - **Everything after it is joined by `&&`,** or is in a pipeline with
    ///   something that is. Then a part after the `cd` runs only if the
    ///   `cd` ran and succeeded. After a `;` or a `||` a part would run in
    ///   one directory if the `cd` succeeded and in another if it failed
    ///   (`cd nowhere && ls; cat ../x`), and after `a || cd x && …` it runs
    ///   whether the `cd` ran or not.
    ///
    /// The shell's own search for directories (`CDPATH`) is taken to be
    /// off, which the shell tool's environment is to ensure
    /// (`docs/build-plan.md`, step 13).
    fn follow_cd(&self, part: &Part, later: &[Part], here: &AbsPath) -> Option<AbsPath> {
        let [target] = part.arguments() else {
            return None;
        };
        let certain = !matches!(part.joiner, Joiner::Pipe | Joiner::Or)
            && target.is_fixed()
            && !target.text.starts_with('-')
            && later.first().is_none_or(|next| next.joiner == Joiner::And)
            && later
                .iter()
                .all(|part| matches!(part.joiner, Joiner::And | Joiner::Pipe));
        if !certain {
            return None;
        }
        self.locate.locate(here, &target.text)
    }

    /// Judges one simple command, run in `here`: how it is allowed, and
    /// what its redirections and the paths it names come to.
    fn judge_part(&self, rules: &RuleSet, part: &Part, here: &AbsPath) -> (Standing, Vec<Finding>) {
        let shown = words_of(part).join(" ");
        let mut findings = Vec::new();
        let mut writes = false;
        for redirect in &part.redirects {
            let null_device = |target: &crate::command::Word| {
                target.text == "/dev/null" && self.shell == ShellSyntax::Posix
            };
            let (target, effect) = match redirect {
                Redirect::Duplicate => continue,
                Redirect::Read(target) | Redirect::Write { target, .. } if null_device(target) => {
                    continue;
                }
                Redirect::Read(target) => (target, Effect::Read),
                Redirect::Write { target, .. } => {
                    writes = true;
                    (target, Effect::Write)
                }
            };
            match self.locate.locate(here, &target.text) {
                Some(path) => findings.push(self.judge_path(rules, &effect, &path, Some(&shown))),
                None => findings.push(Finding::ask(format!(
                    "`{shown}` redirects to or from `{}`, which is not a path that can be checked.",
                    target.text
                ))),
            }
        }

        let named = table::read_only(part, self.shell, self.trusted);
        let standing = match (&named, rules.exec_allowance(part, self.shell)) {
            (Some(_), _) if !writes => Standing::ReadOnly,
            (Some(_), _) | (None, Some(_)) => Standing::Safe,
            (None, None) => Standing::Other,
        };

        match named {
            // A command that only reads: everything it names, and the
            // directory it runs in, is a read like any other.
            Some(reading) => {
                findings.push(mentioned(self.judge_path(
                    rules,
                    &Effect::Read,
                    here,
                    Some(&shown),
                )));
                // A command that prints tracked content reads the whole
                // repository, from wherever in it it runs.
                if reading.repository {
                    let searched = [here, &self.workspace];
                    let held = searched
                        .into_iter()
                        .find_map(|tree| Some((tree, rules.read_held_back_under(tree)?)));
                    if let Some((tree, hit)) = held {
                        findings.push(self.searched(&shown, tree, &hit));
                    }
                }
                for name in reading.named {
                    match self.locate.locate(here, &name.text) {
                        Some(path) => {
                            findings.push(mentioned(self.judge_path(
                                rules,
                                &Effect::Read,
                                &path,
                                Some(&shown),
                            )));
                            // A search reads files nobody named.
                            if name.searched
                                && let Some(hit) = rules.read_held_back_under(&path)
                            {
                                findings.push(self.searched(&shown, &path, &hit));
                            }
                        }
                        // A word that may be a git revision need not be a
                        // path. One that starts at a root, a drive or a
                        // share is a path, and one the harness cannot
                        // follow: a share is refused before it is reached.
                        None if name.must_resolve || starts_elsewhere(&name.text, here.style()) => {
                            findings.push(Finding::ask(format!(
                                "`{shown}` names `{}`, which is not a path that can be checked.",
                                name.text
                            )));
                        }
                        None => {}
                    }
                }
            }
            // Any other command: an argument that names a path a rule
            // holds back, a protected path or a file that looks like a
            // secret makes the command one to ask about, whatever allows
            // the command. The harness's own files stay refused.
            None => {
                for word in part.arguments().iter().filter(|word| word.is_fixed()) {
                    let value = word
                        .text
                        .rsplit_once('=')
                        .map_or(word.text.as_str(), |(_, value)| value);
                    for text in [word.text.as_str(), value] {
                        if text.is_empty() || text.starts_with('-') {
                            continue;
                        }
                        let Some(path) = self.locate.locate(here, text) else {
                            continue;
                        };
                        if let Some(hit) = held_back(rules, &path) {
                            let reason = format!(
                                "`{shown}` names `{path}`, which is protected by {}.",
                                hit.describe()
                            );
                            findings.push(match hit.by {
                                By::Builtin(_) => Finding::Deny {
                                    reason,
                                    built_in: true,
                                },
                                By::Rule(_) => Finding::ask(reason),
                            });
                            break;
                        }
                        if self.is_sensitive(rules, &path) {
                            findings.push(self.beyond(format!(
                                "`{shown}` names `{path}`, which is a protected path or looks \
                                 like a file that holds secrets. Whoever can write a protected \
                                 path can run code later."
                            )));
                            break;
                        }
                    }
                }
                // `git config` writes the repository's configuration,
                // which can name programs for git to run.
                if is_git_config(part)
                    && level::decider(self.level(), Matter::WriteBeyond) != Decider::Holder
                {
                    findings.push(self.beyond(format!(
                        "`{shown}` changes git's configuration, which can name programs for git \
                         to run."
                    )));
                }
            }
        }
        (standing, findings)
    }

    /// The finding for a write beyond what the level lets the holder
    /// decide, made by a command: refused where the level refuses such
    /// writes, and asked about otherwise.
    fn beyond(&self, reason: String) -> Finding {
        if level::decider(self.level(), Matter::WriteBeyond) == Decider::Refused {
            Finding::deny(reason)
        } else {
            Finding::ask(reason)
        }
    }

    /// The finding for a command that reads every file of `tree`, where
    /// something in the tree is held back from reading. Asked, never
    /// refused: the command names the tree, not the file. At
    /// `unrestricted` too: a rule that holds back holds at every level.
    #[expect(
        clippy::unused_self,
        reason = "a judgement like the others, which do read the engine"
    )]
    fn searched(&self, shown: &str, tree: &AbsPath, hit: &Hit<'_>) -> Finding {
        Finding::ask(format!(
            "`{shown}` reads every file it finds in `{tree}`, and {} protects something there. \
             The `search` tool makes the same search and leaves protected files out.",
            hit.describe()
        ))
    }

    // ----------------------------------------------------------- approvals

    /// Records the answer to a question, and says whether the call may
    /// run.
    ///
    /// - "Allow once" adds nothing.
    /// - "Allow for this session" adds the question's rules in memory.
    /// - "Always allow in this project" writes them to the project's
    ///   stored approvals, and they are in force from now on.
    /// - "Deny", and any answer this build does not know, is a denial.
    ///
    /// A choice that was not on offer (a scope with no rule to add) allows
    /// this one call and adds nothing: the approval is never made wider
    /// than what the person was shown.
    ///
    /// Writing the project's file blocks. Call this off the core thread.
    ///
    /// # Errors
    ///
    /// When "always allow in this project" cannot be stored. The call was
    /// still approved by a person: the caller allows it this once, and
    /// shows the error.
    pub fn record(
        &self,
        question: &Question,
        choice: &ApprovalChoice,
    ) -> Result<Recorded, ApprovalError> {
        let allowed = |added| Recorded {
            allowed: true,
            added,
        };
        match choice {
            ApprovalChoice::AllowOnce => Ok(allowed(Vec::new())),
            ApprovalChoice::AllowSession if !question.lifts.is_empty() => {
                let mut rules = self.rules.write().unwrap_or_else(PoisonError::into_inner);
                let now = self.clock.now().to_timestamp();
                let added = rules.session.add(&question.lifts, &now);
                self.recompile(&mut rules);
                Ok(allowed(added))
            }
            ApprovalChoice::AllowProject if !question.lifts.is_empty() => {
                let Some(store) = &self.store else {
                    return Ok(allowed(Vec::new()));
                };
                let added = store.add(&question.lifts)?;
                let mut rules = self.rules.write().unwrap_or_else(PoisonError::into_inner);
                for approval in &added {
                    if !rules.approved.iter().any(|held| held.id == approval.id) {
                        rules.approved.push(approval.clone());
                    }
                }
                self.recompile(&mut rules);
                Ok(allowed(added))
            }
            ApprovalChoice::AllowSession | ApprovalChoice::AllowProject => Ok(allowed(Vec::new())),
            ApprovalChoice::Deny | ApprovalChoice::Unknown(_) => Ok(Recorded {
                allowed: false,
                added: Vec::new(),
            }),
        }
    }

    /// Takes approvals back, by id: session approvals from memory, stored
    /// ones from the project's file. Says how many were in force.
    ///
    /// # Errors
    ///
    /// When the project's file cannot be rewritten. The session's own
    /// approvals have been removed all the same.
    pub fn revoke(&self, ids: &[ApprovalId]) -> Result<usize, ApprovalError> {
        let mut rules = self.rules.write().unwrap_or_else(PoisonError::into_inner);
        let mut removed = rules.session.remove(ids);
        self.recompile(&mut rules);
        if rules
            .approved
            .iter()
            .any(|approval| ids.contains(&approval.id))
        {
            if let Some(store) = &self.store {
                store.remove(ids)?;
            }
            let before = rules.approved.len();
            rules
                .approved
                .retain(|approval| !ids.contains(&approval.id));
            removed += before - rules.approved.len();
            self.recompile(&mut rules);
        }
        Ok(removed)
    }

    /// Compiles the rules again after an approval was added or removed.
    /// Every rule compiled when the session started, and an approval is a
    /// rule the engine made itself, so this cannot fail; if it ever did,
    /// the rules in force stay as they were.
    fn recompile(&self, rules: &mut Rules) {
        if let Ok(compiled) = compile(
            &rules.configured,
            &rules.approved,
            &rules.session,
            &self.workspace,
            self.home.as_ref(),
            self.trusted,
            &self.protected,
        ) {
            rules.compiled = compiled;
        }
    }
}

fn compile(
    configured: &[SourcedRule],
    approved: &[StoredApproval],
    session: &SessionApprovals,
    workspace: &AbsPath,
    home: Option<&AbsPath>,
    trusted: bool,
    protected: &Protected,
) -> Result<RuleSet, Vec<RuleError>> {
    let mut all: Vec<SourcedRule> = configured.to_vec();
    // An approval only ever allows: one that says anything else, in a file
    // somebody edited, is not a rule the user gave by approving.
    let allows = |approval: &&StoredApproval| approval.rule.action == RuleAction::Allow;
    all.extend(
        approved
            .iter()
            .filter(allows)
            .map(|approval| SourcedRule::new(approval.rule.clone(), RuleSource::Approved)),
    );
    all.extend(
        session
            .list()
            .iter()
            .filter(allows)
            .map(|approval| SourcedRule::new(approval.rule.clone(), RuleSource::Session)),
    );
    RuleSet::compile(
        all,
        RuleContext {
            workspace,
            home,
            trusted,
        },
        protected.clone(),
    )
}

/// The finding for a path a command only mentions among its arguments. A
/// rule that denies reading the path makes the command one to ask about,
/// not one to refuse: an argument that reads like a protected file may be
/// a search pattern (`grep .env README.md`), and a person can tell.
/// Nothing is lost by asking, because a command that is asked about never
/// runs unasked. A redirection is not a mention: it is a write for
/// certain, and keeps its denial.
fn mentioned(finding: Finding) -> Finding {
    match finding {
        Finding::Deny {
            reason,
            built_in: false,
        } => Finding::ask(reason),
        other => other,
    }
}

/// Whether a path is part of a git repository's own data, wherever the
/// repository is: anything in a directory named `.git`, a file named `.git`
/// (which points a worktree at its repository), and a file named `HEAD`,
/// which with two directories beside it makes git take a plain directory
/// for a repository and read the `config` in it.
///
/// The repository's configuration can name programs that `git status` and
/// `git diff` run, and those are on the table of commands that only read.
/// So no tool writes any of this unasked, in the workspace's own repository
/// or in one nested in it.
fn is_repository_data(path: &AbsPath) -> bool {
    path.names().any(|name| name.eq_ignore_ascii_case(".git"))
        || path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("HEAD"))
}

/// Whether a simple command is `git config`, however git is named and
/// whatever stands before the subcommand.
///
/// Git's own options come before the subcommand, and some take a value in
/// the next word. With only options this knows, the subcommand is the
/// first word that is not one. With any other option before it, a `config`
/// anywhere counts: asking once too often is the safe side.
fn is_git_config(part: &Part) -> bool {
    const TAKES_A_VALUE: [&str; 7] = [
        "-C",
        "-c",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--config-env",
        "--attr-source",
    ];
    const ALONE: [&str; 12] = [
        "-p",
        "-P",
        "--paginate",
        "--no-pager",
        "--bare",
        "--no-replace-objects",
        "--no-lazy-fetch",
        "--no-optional-locks",
        "--no-advice",
        "--literal-pathspecs",
        "--glob-pathspecs",
        "--noglob-pathspecs",
    ];
    let program = part.program();
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.to_ascii_lowercase();
    if name.strip_suffix(".exe").unwrap_or(&name) != "git" {
        return false;
    }
    let mut words = part.arguments().iter().map(|word| word.text.as_str());
    while let Some(word) = words.next() {
        if TAKES_A_VALUE.contains(&word) {
            words.next();
        } else if ALONE.contains(&word)
            || TAKES_A_VALUE
                .iter()
                .any(|option| option.starts_with("--") && is_given(word, option))
        {
            // An option this knows, with its value attached if it has one.
        } else if word.starts_with('-') {
            return part.arguments().iter().any(|word| word.text == "config");
        } else {
            return word == "config";
        }
    }
    false
}

/// Whether `word` is `--option=value`.
fn is_given(word: &str, option: &str) -> bool {
    word.strip_prefix(option)
        .is_some_and(|rest| rest.starts_with('='))
}

/// Reads the roots of a path set as absolute paths, and writes them back in
/// that form: absolute as it is, `~/…` under the home directory, anything
/// else under the workspace.
fn place_all(
    roots: &mut [String],
    list: &'static str,
    workspace: &AbsPath,
    home: Option<&AbsPath>,
) -> Result<Vec<AbsPath>, StartError> {
    let mut placed = Vec::with_capacity(roots.len());
    for root in roots {
        let path = if let Ok(path) = AbsPath::parse(root, workspace.style()) {
            Some(path)
        } else if root.as_str() == "~" {
            home.cloned()
        } else if let Some(rest) = root.strip_prefix("~/").or_else(|| root.strip_prefix("~\\")) {
            home.and_then(|home| home.join(rest).ok())
        } else {
            workspace.join(root).ok()
        };
        let Some(path) = path else {
            return Err(StartError::Sandbox {
                entry: root.clone(),
                list,
            });
        };
        path.as_str().clone_into(root);
        placed.push(path);
    }
    Ok(placed)
}

/// The strictest of the findings, as the answer for the call.
fn combine(findings: Vec<Finding>, read_only: bool) -> Outcome {
    let mut reasons = Vec::new();
    let mut lifts: Vec<Rule> = Vec::new();
    let mut every_ask_lifts = true;
    for finding in findings {
        match finding {
            Finding::Allow => {}
            Finding::Deny { reason, .. } => return Outcome::Deny { reason },
            Finding::Ask { reason, lift } => {
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
                match lift {
                    Some(rule) if !lifts.contains(&rule) => lifts.push(rule),
                    Some(_) => {}
                    None => every_ask_lifts = false,
                }
            }
        }
    }
    if reasons.is_empty() {
        return Outcome::Allow { read_only };
    }
    // Rules are offered only when they would settle every reason: half an
    // approval would leave the person asked again for the same call.
    if !every_ask_lifts {
        lifts.clear();
    }
    Outcome::Ask(Question { reasons, lifts })
}

/// The `deny` or `ask` rule, or the built-in protection, that holds back
/// reading or writing `path`.
fn held_back<'a>(rules: &'a RuleSet, path: &AbsPath) -> Option<Hit<'a>> {
    [Effect::Read, Effect::Write]
        .iter()
        .filter_map(|effect| rules.path(effect, path))
        .find(|hit| hit.action != RuleAction::Allow)
}

/// Whether a part changes the shell's directory.
fn is_change_of_directory(part: &Part, shell: ShellSyntax) -> bool {
    match shell {
        ShellSyntax::Posix => part.program() == "cd",
        ShellSyntax::PowerShell => matches!(
            part.program().to_ascii_lowercase().as_str(),
            "cd" | "chdir" | "sl" | "set-location"
        ),
        ShellSyntax::Cmd | ShellSyntax::Unknown => false,
    }
}

/// Programs whose arguments are themselves a command, a program's text, or
/// a script in a language of their own. An approval of one covers that
/// exact command line and nothing wider: "always allow `sudo *`" is not
/// what a person meant by approving `sudo apt list`, nor "always allow
/// `find *`" by approving one `find`.
const WRAPPERS: [&str; 56] = [
    "sudo",
    "doas",
    "su",
    "env",
    "xargs",
    "nohup",
    "nice",
    "ionice",
    "timeout",
    "watch",
    "time",
    "stdbuf",
    "setsid",
    "chroot",
    "unshare",
    "flock",
    "strace",
    "ltrace",
    "script",
    "busybox",
    "sh",
    "bash",
    "dash",
    "zsh",
    "ksh",
    "fish",
    "pwsh",
    "powershell",
    "cmd",
    "python",
    "python3",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "lua",
    "awk",
    "ssh",
    "docker",
    "podman",
    "kubectl",
    "find",
    "sed",
    "tar",
    "rsync",
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "pipx",
    "parallel",
    "watchexec",
    "entr",
    "git-shell",
];

/// Programs that do their work through subcommands. An approval of one
/// names the subcommand; where there is none in second place (an option
/// comes first, as in `git -C dir status`), it covers the exact line.
const HAS_SUBCOMMANDS: [&str; 22] = [
    "git",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "go",
    "pip",
    "pip3",
    "gh",
    "dotnet",
    "apt",
    "apt-get",
    "brew",
    "rustup",
    "poetry",
    "uv",
    "hg",
    "svn",
    "conda",
    "systemctl",
    "dnf",
    "just",
];

/// The allow rule an approval of this command adds: the program and its
/// subcommand, with any further words. `None` when no pattern says that
/// safely.
fn approval_rule(part: &Part) -> Option<Rule> {
    let plain = |text: &str| {
        !text.is_empty()
            && !text.chars().any(|character| {
                character.is_whitespace() || matches!(character, '*' | '?' | '[' | ']')
            })
    };
    let program = part.words.first()?;
    if !program.is_fixed() || program.home || !plain(&program.text) {
        return None;
    }
    let name = program
        .text
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&program.text)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    // A second word is a subcommand when it reads like one: `test` in
    // `cargo test`, not `-la` or `src/main.rs`.
    let subcommand = part.words.get(1).filter(|word| {
        word.is_fixed()
            && !word.home
            && word
                .text
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
            && word.text.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
    });
    let exact_line = WRAPPERS.contains(&name)
        || (subcommand.is_none() && HAS_SUBCOMMANDS.contains(&name) && part.words.len() > 1);
    let pattern = if exact_line {
        // The whole line, word for word, and no more.
        if !part
            .words
            .iter()
            .all(|word| word.is_fixed() && !word.home && plain(&word.text))
        {
            return None;
        }
        words_of(part).join(" ")
    } else {
        match subcommand {
            Some(subcommand) => format!("{} {} *", program.text, subcommand.text),
            None => format!("{} *", program.text),
        }
    };
    Some(Rule {
        effect: Effect::Exec,
        pattern,
        action: RuleAction::Allow,
    })
}

/// A glob that matches exactly `text`: every character that a pattern
/// would read as special is written as a set of one.
fn literal_name(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '*' | '?' | '[' | '{' | '}' => {
                pattern.push('[');
                pattern.push(character);
                pattern.push(']');
            }
            ']' => pattern.push_str("[]]"),
            other => pattern.push(other),
        }
    }
    pattern
}

/// A path glob that matches exactly `path`.
fn literal_pattern(path: &AbsPath) -> String {
    literal_name(path.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(command: &str) -> Part {
        match analyse(command, ShellSyntax::Posix) {
            Analysis::Parts(mut parts) => parts.remove(0),
            Analysis::NotModelled { why } => panic!("{command:?}: {why}"),
        }
    }

    #[test]
    fn an_approval_covers_the_program_and_its_subcommand() {
        for (command, expected) in [
            ("cargo build --release", Some("cargo build *")),
            ("cargo test -p cortex-policy", Some("cargo test *")),
            ("git push origin main", Some("git push *")),
            ("npm run build", Some("npm run *")),
            ("make", Some("make *")),
            ("make -j8 all", Some("make *")),
            ("ls -la src", Some("ls *")),
            ("./scripts/build.sh fast", Some("./scripts/build.sh fast *")),
            ("rm src/main.rs", Some("rm *")),
            // A program that works through subcommands, with an option
            // where the subcommand would be: the exact line.
            ("git -C sub status", Some("git -C sub status")),
            ("cargo --locked test", Some("cargo --locked test")),
            ("git", Some("git *")),
            // Programs with a language of their own in their arguments.
            ("find . -name x -delete", Some("find . -name x -delete")),
            ("sed -i s/a/b/ f", Some("sed -i s/a/b/ f")),
            ("tar xf a.tar", Some("tar xf a.tar")),
            ("npx cowsay hi", Some("npx cowsay hi")),
            ("pytest tests/unit", Some("pytest *")),
            ("go test ./...", Some("go test *")),
            // A program that runs its arguments: the exact line only.
            ("sudo apt list", Some("sudo apt list")),
            ("env FOO=1 make", Some("env FOO=1 make")),
            ("timeout 5 cargo test", Some("timeout 5 cargo test")),
            ("bash -c ls", Some("bash -c ls")),
            ("python3 script.py --flag", Some("python3 script.py --flag")),
            ("/usr/bin/python3 x.py", Some("/usr/bin/python3 x.py")),
            ("xargs rm", Some("xargs rm")),
            // Which cannot be said in a pattern when a word has a space.
            ("bash -c 'rm -rf x'", None),
            ("python3 -c 'import os, sys'", None),
            // Word for word, a quoted word with no space in it included.
            ("python3 -c 'print(1)'", Some("python3 -c print(1)")),
            ("sudo rm *", None),
        ] {
            let rule = approval_rule(&part(command));
            assert_eq!(
                rule.as_ref().map(|rule| rule.pattern.as_str()),
                expected,
                "{command:?}"
            );
            if let Some(rule) = rule {
                assert_eq!(rule.effect, Effect::Exec);
                assert_eq!(rule.action, RuleAction::Allow);
                // The rule allows the command it was made from.
                let pattern = crate::command::pattern::ExecPattern::parse(&rule.pattern).unwrap();
                assert!(
                    pattern.allows(&part(command), ShellSyntax::Posix),
                    "{command:?}"
                );
            }
        }
    }

    #[test]
    fn a_literal_pattern_matches_only_its_own_text() {
        use crate::glob::NamePattern;
        for text in ["docs/find", "a*b", "what?", "[x]", "{a,b}", "plain", "]["] {
            let pattern = NamePattern::parse(&literal_name(text)).unwrap();
            assert!(pattern.matches(text), "{text:?}");
            // And nothing one character away from it.
            for (index, _) in text.char_indices() {
                let mut other = text.to_owned();
                other.replace_range(index..index + 1, "x");
                if other != text {
                    assert!(!pattern.matches(&other), "{text:?} matched {other:?}");
                }
            }
        }
        assert!(
            !NamePattern::parse(&literal_name("a*b"))
                .unwrap()
                .matches("axb")
        );
        assert!(
            !NamePattern::parse(&literal_name("what?"))
                .unwrap()
                .matches("whatx")
        );
    }
}
