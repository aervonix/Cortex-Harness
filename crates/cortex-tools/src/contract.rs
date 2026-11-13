//! The tool contract (`docs/architecture/05-tools.md`, "The tool trait").
//!
//! A call has two phases. [`Tool::plan`] reads the arguments and says what
//! the call would do: its [`Effect`]s, its [`Concurrency`] and a one-line
//! summary. Nothing has happened yet, so the permission engine, the sandbox
//! and the scheduler can all act on the [`Plan`]. [`Tool::run`] then does
//! the work.
//!
//! Built-in tools, extension tools and MCP tools all implement [`Tool`], so
//! one pipeline applies to all three.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use cortex_base::{AbsPath, ResolvedPath};
use cortex_protocol::lease::{Effect as EffectKind, RuleAction};

use crate::context::{PlanCx, RunCx};
use crate::result::{ToolError, ToolResult};

/// A JSON value: tool arguments, argument schemas and structured payloads.
pub type Json = serde_json::Value;

/// A future that owns its state and can move between threads, as
/// [`Tool::run`] returns it.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One tool: built in, from an extension, or from an MCP server.
pub trait Tool: Send + Sync {
    /// The tool's name, description, argument schema, default permission
    /// and concurrency class. It never changes while the tool exists.
    fn spec(&self) -> &ToolSpec;

    /// Validates the arguments and declares what the call would do. **No
    /// side effects.**
    ///
    /// It may ask the file system where a path leads, through the resolver
    /// in `cx`, and nothing else. That is a handful of system calls and it
    /// blocks, so a caller on an async thread runs it through
    /// [`run_blocking`](crate::run_blocking).
    ///
    /// An error is for the model: it says what was wrong with the call and
    /// what to try. The caller turns it into the call's result with
    /// [`ToolError::into_result`].
    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError>;

    /// Does the work of a plan this tool made.
    ///
    /// - It stops promptly when `cx.cancel` fires, and still returns a
    ///   result: one whose outcome is `interrupted`.
    /// - A failure is a result too ([`ToolResult::error`]), never a panic.
    /// - Output that should reach the user while the tool runs goes to
    ///   `out`.
    ///
    /// Callers do not call this directly: [`execute`](crate::execute) runs
    /// it and then applies the output policy, which is what bounds the
    /// result of every tool in the same way.
    fn run<'a>(
        &'a self,
        plan: Plan,
        cx: &'a RunCx,
        out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult>;

    /// What a planned call would change in a file, for a question put to
    /// the user about it: the work that finds out, worked out the way
    /// [`run`](Self::run) would make the change, and changing nothing.
    /// `None`, the default, for a tool that changes no file.
    ///
    /// The work reads the file, so a caller on an async thread runs it
    /// through [`run_blocking`](crate::run_blocking). Calling this method
    /// does no work itself. A file the read gate in `cx` keeps from the
    /// agent is not shown: the question is put to a person, and is no way
    /// round the read rules.
    fn preview(&self, plan: &Plan, cx: &RunCx) -> Option<Preview> {
        let _ = (plan, cx);
        None
    }
}

/// The work that makes a [`ChangePreview`] ([`Tool::preview`]). It reads
/// the file it is about, and blocks.
pub type Preview = Box<dyn FnOnce() -> ChangePreview + Send>;

/// What a planned call would change in a file, for a question put to the
/// user (`approval/request`'s `diff` detail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangePreview {
    /// The file, as an absolute path in the system's own form.
    pub path: String,
    /// The change, as a unified diff of the lines it changes. Empty when it
    /// is left out.
    pub diff: String,
    /// Why the diff is left out, in a sentence: it is larger than a request
    /// carries, or the change cannot be worked out before the call runs
    /// (the call will then say why it failed). `None` when `diff` is the
    /// whole change.
    pub omitted: Option<String>,
}

/// What a tool is, as the model and the permission engine are told.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolSpec {
    /// The name the model calls it by. For a dynamic tool this is the name
    /// inside its source: the registry adds the `source/` prefix.
    pub name: String,
    /// What it does and when to use it, written for the model.
    pub description: String,
    /// The JSON Schema of its arguments.
    pub parameters: Json,
    /// What the permission engine decides for a call that no rule and no
    /// level decides: `allow` for the built-in tools whose effects are
    /// checked one by one, `ask` for tools from extensions and MCP servers.
    /// Every effect of a plan is checked whatever this says.
    pub permission: RuleAction,
    /// How calls of this tool are scheduled, unless a plan says otherwise.
    pub concurrency: ConcurrencyClass,
}

impl ToolSpec {
    /// A spec with the defaults of a tool that only reads: permission
    /// `allow`, class [`ConcurrencyClass::ReadOnly`].
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Json) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            permission: RuleAction::Allow,
            concurrency: ConcurrencyClass::ReadOnly,
        }
    }

    /// Sets the default permission.
    #[must_use]
    pub fn permission(mut self, permission: RuleAction) -> Self {
        self.permission = permission;
        self
    }

    /// Sets the concurrency class.
    #[must_use]
    pub fn concurrency(mut self, concurrency: ConcurrencyClass) -> Self {
        self.concurrency = concurrency;
        self
    }
}

/// How a tool's calls are scheduled
/// (`docs/architecture/03-agent-loop.md`, "Tool rounds").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConcurrencyClass {
    /// Changes nothing. Runs beside other calls.
    ReadOnly,
    /// Changes the paths it names. Calls on one path run one at a time, in
    /// the model's order; calls on different paths run together.
    Keyed,
    /// Runs alone: it waits for running calls, and later calls wait for it.
    Exclusive,
}

/// How one planned call is scheduled: its class, with the paths of a keyed
/// call. A tool may refine its class per call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Concurrency {
    /// Changes nothing.
    ReadOnly,
    /// Changes these paths, each a real location.
    Keyed(Vec<AbsPath>),
    /// Runs alone.
    Exclusive,
}

impl Concurrency {
    /// The class, without the paths.
    #[must_use]
    pub fn class(&self) -> ConcurrencyClass {
        match self {
            Self::ReadOnly => ConcurrencyClass::ReadOnly,
            Self::Keyed(_) => ConcurrencyClass::Keyed,
            Self::Exclusive => ConcurrencyClass::Exclusive,
        }
    }
}

/// One thing a planned call would do. The permission engine decides each
/// (`docs/architecture/06-permissions-and-sandbox.md`, "What is checked").
///
/// A path in an effect is a [`ResolvedPath`]: the real location, with every
/// link followed, and whether it is inside the workspace. A rule is matched
/// against that, never against what the model wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Reads one file, or the names in one directory.
    Read(ResolvedPath),
    /// Reads the tree under a directory: a walk. The walk does not follow
    /// links, so it stays under this path, and each entry it meets is put to
    /// the [`ReadGate`](crate::ReadGate) before it is read.
    ReadTree(ResolvedPath),
    /// Creates or changes one file.
    Write(ResolvedPath),
    /// Runs a command.
    Exec {
        /// The full command line.
        command: String,
        /// The directory it runs in.
        cwd: ResolvedPath,
    },
    /// Reaches the network.
    Network(NetworkTarget),
    /// Calls an extension or MCP tool, by its full name, `source/tool`.
    Ext {
        /// The tool's full name.
        tool: String,
    },
}

/// Where a network effect goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkTarget {
    /// One host.
    Host(String),
    /// Any host: the call cannot say in advance.
    Any,
}

impl Effect {
    /// The effect's kind, as rules and the protocol name it. A walk is a
    /// `read`.
    #[must_use]
    pub fn kind(&self) -> EffectKind {
        match self {
            Self::Read(_) | Self::ReadTree(_) => EffectKind::Read,
            Self::Write(_) => EffectKind::Write,
            Self::Exec { .. } => EffectKind::Exec,
            Self::Network(_) => EffectKind::Network,
            Self::Ext { .. } => EffectKind::Ext,
        }
    }

    /// The path the effect is about, for the effects that have one.
    #[must_use]
    pub fn path(&self) -> Option<&ResolvedPath> {
        match self {
            Self::Read(path) | Self::ReadTree(path) | Self::Write(path) => Some(path),
            Self::Exec { .. } | Self::Network(_) | Self::Ext { .. } => None,
        }
    }

    /// What the effect is aimed at, as text for an approval prompt: the
    /// path as it is shown to the model, the command line, the host (`*`
    /// for any), or the tool's name.
    #[must_use]
    pub fn target(&self) -> String {
        match self {
            Self::Read(path) | Self::Write(path) => path.display(),
            Self::ReadTree(path) => {
                let shown = path.display();
                if shown.ends_with('/') {
                    format!("{shown}**")
                } else {
                    format!("{shown}/**")
                }
            }
            Self::Exec { command, .. } => command.clone(),
            Self::Network(NetworkTarget::Host(host)) => host.clone(),
            Self::Network(NetworkTarget::Any) => "*".to_owned(),
            Self::Ext { tool } => tool.clone(),
        }
    }

    /// Whether the effect only reads. A call with none but such effects can
    /// start before the model's response has ended
    /// (`docs/architecture/15-efficiency.md`, "Starting tools early").
    #[must_use]
    pub fn is_read(&self) -> bool {
        matches!(self, Self::Read(_) | Self::ReadTree(_))
    }
}

/// What a call would do, declared before anything runs.
///
/// A plan also carries the validated arguments, which only the tool that
/// made the plan can read. A plan is run by the tool that made it.
pub struct Plan {
    /// What the call would do. An empty list is a call that touches nothing
    /// outside the session.
    pub effects: Vec<Effect>,
    /// How the call is scheduled.
    pub concurrency: Concurrency,
    /// One line for approval prompts and the UI: `Read src/main.rs`.
    pub summary: String,
    /// Whether the call changes the session itself: the plan the user sees,
    /// the proposals waiting for review. Such a call touches nothing
    /// outside the session, so it has no effects, and it is still not safe
    /// to run for a response that may be thrown away, or beside another
    /// call that changes the session. See [`Plan::changes_session`].
    pub changes_session: bool,
    args: Box<dyn Any + Send + Sync>,
}

impl Plan {
    /// A plan with no effects, scheduled as read-only. `args` is whatever
    /// the tool wants back in [`Tool::run`]: its arguments, validated.
    pub fn new(summary: impl Into<String>, args: impl Any + Send + Sync) -> Self {
        Self {
            effects: Vec::new(),
            concurrency: Concurrency::ReadOnly,
            summary: summary.into(),
            changes_session: false,
            args: Box::new(args),
        }
    }

    /// Adds an effect.
    #[must_use]
    pub fn with_effect(mut self, effect: Effect) -> Self {
        self.effects.push(effect);
        self
    }

    /// Sets how the call is scheduled.
    #[must_use]
    pub fn with_concurrency(mut self, concurrency: Concurrency) -> Self {
        self.concurrency = concurrency;
        self
    }

    /// Marks the call as one that changes the session.
    #[must_use]
    pub fn with_session_change(mut self) -> Self {
        self.changes_session = true;
        self
    }

    /// The validated arguments, if they are a `T`.
    #[must_use]
    pub fn args<T: Any>(&self) -> Option<&T> {
        self.args.downcast_ref()
    }

    /// Takes the validated arguments out. A plan made by another tool, whose
    /// arguments are not a `T`, is
    /// [`ToolError::Internal`]: a bug in the caller, reported and not
    /// panicked on.
    pub fn into_args<T: Any>(self) -> Result<T, ToolError> {
        match self.args.downcast::<T>() {
            Ok(args) => Ok(*args),
            Err(_) => Err(ToolError::Internal(format!(
                "the plan `{}` was made by another tool",
                self.summary
            ))),
        }
    }

    /// Whether the call would change anything: whether it has an effect that
    /// is not a read. A call without one is safe to start early and to
    /// repeat.
    #[must_use]
    pub fn has_side_effects(&self) -> bool {
        self.effects.iter().any(|effect| !effect.is_read())
    }

    /// Whether the call changes the session itself, as `plan` and
    /// `remember` do.
    ///
    /// The loop reads this beside [`has_side_effects`](Self::has_side_effects)
    /// (`docs/architecture/03-agent-loop.md`, "Tool rounds"):
    ///
    /// - such a call never starts before its response has ended. A response
    ///   can fail half-way and be sent again, and a plan that was replaced,
    ///   or a memory that was proposed, for a message that no longer exists
    ///   would stay behind;
    /// - two such calls [conflict](Self::conflicts_with), so they run one at
    ///   a time in the model's order: of two `plan` calls in one response
    ///   the second is the one that stands.
    ///
    /// It is not an [`Effect`]: nothing outside the session is touched, so
    /// there is nothing for the permission engine to decide.
    #[must_use]
    pub fn changes_session(&self) -> bool {
        self.changes_session
    }

    /// Whether this call and `other` must not run at the same time, so that
    /// the scheduler runs them in the model's order.
    ///
    /// - An exclusive call conflicts with every call.
    /// - Two calls that both [change the session](Self::changes_session)
    ///   conflict.
    /// - Two keyed calls conflict when they share a path, or when a path of
    ///   one is inside a path of the other.
    /// - A keyed call conflicts with a read-only call that reads one of its
    ///   paths, a directory that holds one, or something inside one:
    ///   `edit a.rs` then `read a.rs` must see the edit, and `write
    ///   src/new.rs` then `list src` must see the file.
    /// - Two read-only calls never conflict.
    #[must_use]
    pub fn conflicts_with(&self, other: &Self) -> bool {
        if self.changes_session && other.changes_session {
            return true;
        }
        match (&self.concurrency, &other.concurrency) {
            (Concurrency::Exclusive, _) | (_, Concurrency::Exclusive) => true,
            (Concurrency::ReadOnly, Concurrency::ReadOnly) => false,
            (Concurrency::Keyed(mine), Concurrency::Keyed(theirs)) => mine
                .iter()
                .any(|path| theirs.iter().any(|other| related(path, other))),
            (Concurrency::Keyed(keys), Concurrency::ReadOnly) => reads_any(other, keys),
            (Concurrency::ReadOnly, Concurrency::Keyed(keys)) => reads_any(self, keys),
        }
    }
}

/// Whether one path is the other, or lies inside it.
fn related(first: &AbsPath, second: &AbsPath) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

/// Whether a plan that is scheduled as read-only reads any of `keys`, a
/// directory one of them is in, or something inside one of them. A read of
/// a directory is its listing, which a write below it can change, so it is
/// treated like a walk.
///
/// A command reads whatever it likes, and its plan cannot say what. So a
/// plan with an `exec` effect that the permission engine has marked
/// read-only is taken to read every key: `edit a.rs` followed by a
/// read-only `cat a.rs` sees the edit.
fn reads_any(plan: &Plan, keys: &[AbsPath]) -> bool {
    plan.effects.iter().any(|effect| match effect {
        Effect::Read(path) | Effect::ReadTree(path) => {
            keys.iter().any(|key| related(key, path.abs()))
        }
        Effect::Exec { .. } => true,
        _ => false,
    })
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("summary", &self.summary)
            .field("effects", &self.effects)
            .field("concurrency", &self.concurrency)
            .field("changes_session", &self.changes_session)
            .finish_non_exhaustive()
    }
}

/// Where a running tool sends output that the user should see as it
/// arrives. Front ends receive it as `tool_output` deltas. It is a display
/// convenience: the tool's result is what the model and the log get.
pub trait ToolOutput: Send {
    /// The next piece of output.
    fn output(&mut self, text: &str);
}

/// A [`ToolOutput`] that drops what it is given, for callers with nobody
/// watching.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOutput;

impl ToolOutput for NoOutput {
    fn output(&mut self, _text: &str) {}
}

impl ToolOutput for String {
    fn output(&mut self, text: &str) {
        self.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(text: &str) -> ResolvedPath {
        // A resolver needs a real directory. The tests of paths on a real
        // file system are in `tests/it`; here the workspace is wherever the
        // tests run, and the paths below need not exist.
        let here = std::env::current_dir().expect("a current directory");
        let resolver = cortex_base::PathResolver::new(&here, cortex_base::PathOptions::new())
            .expect("the current directory is a workspace");
        resolver.resolve(text).expect("a plain relative path")
    }

    fn keyed(path: &ResolvedPath) -> Plan {
        Plan::new("write", ())
            .with_effect(Effect::Write(path.clone()))
            .with_concurrency(Concurrency::Keyed(vec![path.abs().clone()]))
    }

    fn reads(path: &ResolvedPath) -> Plan {
        Plan::new("read", ()).with_effect(Effect::Read(path.clone()))
    }

    #[test]
    fn a_plan_keeps_its_arguments_for_the_tool_that_made_it() {
        let plan = Plan::new("count", 7u32);
        assert_eq!(plan.args::<u32>(), Some(&7));
        assert_eq!(plan.args::<String>(), None);
        assert_eq!(plan.into_args::<u32>().expect("the same type"), 7);

        let wrong = Plan::new("count", 7u32).into_args::<String>();
        assert!(matches!(wrong, Err(ToolError::Internal(_))));
    }

    #[test]
    fn only_reads_are_free_of_side_effects() {
        let file = resolved("no-such-dir-for-tests/a.txt");
        assert!(!Plan::new("nothing", ()).has_side_effects());
        assert!(!reads(&file).has_side_effects());
        assert!(
            !Plan::new("walk", ())
                .with_effect(Effect::ReadTree(file.clone()))
                .has_side_effects()
        );
        assert!(keyed(&file).has_side_effects());
        for effect in [
            Effect::Exec {
                command: "ls".to_owned(),
                cwd: file.clone(),
            },
            Effect::Network(NetworkTarget::Any),
            Effect::Ext {
                tool: "docs/find".to_owned(),
            },
        ] {
            assert!(
                Plan::new("effect", ())
                    .with_effect(effect)
                    .has_side_effects()
            );
        }
    }

    #[test]
    fn conflicts_follow_the_classes_and_the_paths() {
        let a = resolved("no-such-dir-for-tests/a.txt");
        let b = resolved("no-such-dir-for-tests/b.txt");
        let dir = resolved("no-such-dir-for-tests");
        let elsewhere = resolved("another-missing-dir");
        let exclusive = || Plan::new("shell", ()).with_concurrency(Concurrency::Exclusive);
        let walk = |tree: &ResolvedPath| {
            Plan::new("search", ()).with_effect(Effect::ReadTree(tree.clone()))
        };

        // Read-only calls run together.
        assert!(!reads(&a).conflicts_with(&reads(&a)));
        assert!(!walk(&dir).conflicts_with(&reads(&a)));
        // An exclusive call runs alone.
        assert!(exclusive().conflicts_with(&reads(&a)));
        assert!(reads(&a).conflicts_with(&exclusive()));
        assert!(exclusive().conflicts_with(&exclusive()));
        // Keyed calls conflict on a shared path only.
        assert!(keyed(&a).conflicts_with(&keyed(&a)));
        assert!(!keyed(&a).conflicts_with(&keyed(&b)));
        // A read of a path being written waits its turn, both ways round.
        assert!(keyed(&a).conflicts_with(&reads(&a)));
        assert!(reads(&a).conflicts_with(&keyed(&a)));
        assert!(!keyed(&a).conflicts_with(&reads(&b)));
        // So does a walk of a tree that holds it.
        assert!(walk(&dir).conflicts_with(&keyed(&a)));
        assert!(keyed(&a).conflicts_with(&walk(&dir)));
        assert!(!keyed(&a).conflicts_with(&walk(&elsewhere)));
        // And a listing of the directory it is in: a new file must show.
        assert!(keyed(&a).conflicts_with(&reads(&dir)));
        assert!(reads(&dir).conflicts_with(&keyed(&a)));
        assert!(!keyed(&a).conflicts_with(&reads(&elsewhere)));
        // A keyed call on a directory conflicts with calls on what is in it.
        assert!(keyed(&dir).conflicts_with(&keyed(&a)));
        assert!(keyed(&a).conflicts_with(&keyed(&dir)));
        assert!(keyed(&dir).conflicts_with(&reads(&a)));
        assert!(!keyed(&dir).conflicts_with(&keyed(&elsewhere)));
    }

    /// A shell command that the permission engine has marked read-only runs
    /// beside reads and beside other such commands, and waits for every
    /// write: its plan cannot say which files it reads.
    #[test]
    fn a_command_marked_read_only_waits_for_every_write() {
        let a = resolved("no-such-dir-for-tests/a.txt");
        let elsewhere = resolved("another-missing-dir/b.txt");
        let root = resolved(".");
        let command = || {
            Plan::new("shell", ())
                .with_effect(Effect::Exec {
                    command: "cat no-such-dir-for-tests/a.txt".to_owned(),
                    cwd: root.clone(),
                })
                .with_effect(Effect::Write(root.clone()))
                .with_concurrency(Concurrency::ReadOnly)
        };
        assert!(keyed(&a).conflicts_with(&command()));
        assert!(command().conflicts_with(&keyed(&a)));
        assert!(keyed(&elsewhere).conflicts_with(&command()));
        assert!(!command().conflicts_with(&reads(&a)));
        assert!(!command().conflicts_with(&command()));
        // As planned, before the engine marks it, it runs alone.
        let exclusive = command().with_concurrency(Concurrency::Exclusive);
        assert!(exclusive.conflicts_with(&reads(&a)));
    }

    #[test]
    fn a_call_that_changes_the_session_conflicts_only_with_another_that_does() {
        let a = resolved("no-such-dir-for-tests/a.txt");
        let session = || Plan::new("plan", ()).with_session_change();
        assert!(!Plan::new("nothing", ()).changes_session());
        assert!(session().changes_session());
        // It has no effect outside the session, and is scheduled as
        // read-only: the marker is what says it is not free to repeat.
        assert!(!session().has_side_effects());
        assert_eq!(session().concurrency, Concurrency::ReadOnly);
        // Two of them run in the model's order.
        assert!(session().conflicts_with(&session()));
        // Beside anything else they run as any read-only call does.
        assert!(!session().conflicts_with(&reads(&a)));
        assert!(!reads(&a).conflicts_with(&session()));
        assert!(!session().conflicts_with(&keyed(&a)));
        assert!(!keyed(&a).conflicts_with(&session()));
        assert!(!session().conflicts_with(&Plan::new("nothing", ())));
        let exclusive = Plan::new("shell", ()).with_concurrency(Concurrency::Exclusive);
        assert!(session().conflicts_with(&exclusive));
    }

    #[test]
    fn an_effect_names_its_kind_and_its_target() {
        let file = resolved("no-such-dir-for-tests/a.txt");
        let read = Effect::Read(file.clone());
        assert_eq!(read.kind(), EffectKind::Read);
        assert_eq!(read.target(), "no-such-dir-for-tests/a.txt");
        assert_eq!(read.path(), Some(&file));

        let walk = Effect::ReadTree(resolved("no-such-dir-for-tests"));
        assert_eq!(walk.kind(), EffectKind::Read);
        assert_eq!(walk.target(), "no-such-dir-for-tests/**");

        assert_eq!(Effect::Write(file.clone()).kind(), EffectKind::Write);
        let exec = Effect::Exec {
            command: "cargo test".to_owned(),
            cwd: file,
        };
        assert_eq!(exec.kind(), EffectKind::Exec);
        assert_eq!(exec.target(), "cargo test");
        assert_eq!(exec.path(), None);
        assert_eq!(Effect::Network(NetworkTarget::Any).target(), "*");
        assert_eq!(
            Effect::Network(NetworkTarget::Host("example.com".to_owned())).kind(),
            EffectKind::Network
        );
        let ext = Effect::Ext {
            tool: "docs/find".to_owned(),
        };
        assert_eq!(ext.kind(), EffectKind::Ext);
        assert_eq!(ext.target(), "docs/find");
    }

    #[test]
    fn a_spec_starts_as_a_read_only_tool() {
        let spec = ToolSpec::new("peek", "Looks.", serde_json::json!({"type": "object"}));
        assert_eq!(spec.permission, RuleAction::Allow);
        assert_eq!(spec.concurrency, ConcurrencyClass::ReadOnly);
        let spec = spec
            .permission(RuleAction::Ask)
            .concurrency(ConcurrencyClass::Exclusive);
        assert_eq!(spec.permission, RuleAction::Ask);
        assert_eq!(spec.concurrency, ConcurrencyClass::Exclusive);
        assert_eq!(Concurrency::Exclusive.class(), ConcurrencyClass::Exclusive);
        assert_eq!(
            Concurrency::Keyed(Vec::new()).class(),
            ConcurrencyClass::Keyed
        );
        assert_eq!(Concurrency::ReadOnly.class(), ConcurrencyClass::ReadOnly);
    }
}
