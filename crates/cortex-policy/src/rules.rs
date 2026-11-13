//! Permission rules and their evaluation
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Rules").
//!
//! A rule is the protocol's [`Rule`]: an effect, a pattern and an action.
//! It is read only through the protocol's strict reader, so a rule with a
//! field or a value this build does not know never reaches this module
//! (`docs/decisions/0028-enforced-values-fail-closed.md`). Here rules are
//! compiled once into a [`RuleSet`], and then asked about one thing at a
//! time.
//!
//! # Evaluation
//!
//! All the rules that match are collected, and **`deny` beats `ask` beats
//! `allow`**. The order of the rules does not matter, so a broad allow can
//! never hide a deny, and adding an allow never changes a deny. When no
//! rule matches, the answer is "no rule", and the level decides
//! ([`crate::level`]).
//!
//! # Who wrote a rule
//!
//! Every rule comes with its [`RuleSource`]. Two things follow from it:
//!
//! - **A project's allow rules wait for trust.** In a project the user has
//!   not trusted, a rule from the project's own files, or from what was
//!   learned or shared for the project, takes effect only if it denies or
//!   asks. Its allows are set aside ([`RuleSet::withheld`]) for a front end
//!   to show when it asks about trust. The configuration loader already
//!   holds such rules back; this is the same rule again where the rules
//!   are enforced, so that nothing upstream has to be right for it to hold.
//! - **Layers cannot weaken each other.** A managed `deny` or `ask` stands
//!   whatever a user rule allows, and a user's whatever a project's allows,
//!   because that is what "deny beats ask beats allow" gives without any
//!   notion of rank.
//!
//! # Matching, and case
//!
//! | Effect | The pattern is | Matched against |
//! |---|---|---|
//! | `read`, `write` | A path glob ([`crate::glob`]). | The resolved path. |
//! | `exec` | Words ([`ExecPattern`]). | The analysed command; a deny or an ask also against the raw line. |
//! | `network` | A host, with `*` for part of one. | The host, in lower case. |
//! | `ext` | A tool's full name, with `*` for part of one. | `source/tool`. |
//!
//! **A rule that denies or asks is matched as widely as it can be**, and a
//! rule that allows as narrowly. On Windows a deny or an ask matches a path
//! without regard to case, and also exactly; an allow matches exactly,
//! against the path as the file system spells it. For commands, a deny or
//! an ask matches wherever its words appear in the line; an allow only a
//! whole simple command of a line the analysis models.
//!
//! # Built-in protection
//!
//! The harness's own files are protected by rules nobody wrote and nobody
//! can remove ([`Protected`]): its configuration directory, where
//! credentials are kept; its state directory, where trust and stored
//! approvals are kept; and the session logs of other projects. They deny,
//! so no allow rule and no level lifts them.

use cortex_base::AbsPath;
use cortex_protocol::ids::ProjectKey;
use cortex_protocol::lease::{Effect, Rule, RuleAction};

use crate::command::pattern::ExecPattern;
use crate::command::{Part, ShellSyntax};
use crate::glob::{Anchors, Case, NamePattern, PathGlob};

/// Who wrote a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleSource {
    /// The managed configuration of a shared machine.
    Managed,
    /// The user's configuration.
    User,
    /// A flag on the command line.
    CommandLine,
    /// The project's own configuration files.
    Project,
    /// What was learned or shared for the project and accepted for its
    /// safe list (`docs/architecture/14-adaptation.md`).
    Learned,
    /// "Always allow in this project": the user's stored approval.
    Approved,
    /// "Allow for this session": held in memory.
    Session,
    /// The session's lease.
    Lease,
}

impl RuleSource {
    /// Whether an allow from this source takes effect only once the user
    /// has trusted the project.
    #[must_use]
    pub fn allow_needs_trust(self) -> bool {
        matches!(self, Self::Project | Self::Learned)
    }

    /// The source in words, for an explanation: "the user's configuration".
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Managed => "the managed configuration",
            Self::User => "your configuration",
            Self::CommandLine => "the command line",
            Self::Project => "the project's configuration",
            Self::Learned => "the project's learned commands",
            Self::Approved => "an approval you stored for this project",
            Self::Session => "an approval you gave for this session",
            Self::Lease => "the session's lease",
        }
    }
}

/// A rule and who wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedRule {
    /// The rule.
    pub rule: Rule,
    /// Who wrote it.
    pub source: RuleSource,
}

impl SourcedRule {
    /// A rule from a source.
    #[must_use]
    pub fn new(rule: Rule, source: RuleSource) -> Self {
        Self { rule, source }
    }

    /// The rule in a line: `deny write **/.env*`.
    #[must_use]
    pub fn show(&self) -> String {
        format!(
            "{} {} {}",
            self.rule.action.as_str(),
            self.rule.effect.as_str(),
            self.rule.pattern
        )
    }
}

/// Why a rule could not be compiled. A set with such a rule is refused
/// whole: a deny that cannot be read must not be dropped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the rule `{}` from {} cannot be used: {problem}", .rule.show(), .rule.source.describe())]
pub struct RuleError {
    /// The rule.
    pub rule: SourcedRule,
    /// What is wrong with it.
    pub problem: String,
}

/// What rules are compiled against.
#[derive(Debug, Clone, Copy)]
pub struct RuleContext<'a> {
    /// The workspace root, resolved. Relative path patterns start here.
    pub workspace: &'a AbsPath,
    /// The home directory, for `~`.
    pub home: Option<&'a AbsPath>,
    /// Whether the user has trusted the project.
    pub trusted: bool,
}

/// The harness's own files, which nothing may reach through a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Protected {
    /// Trees that may be neither read nor written.
    closed: Vec<AbsPath>,
    /// Trees that may not be written.
    read_only: Vec<AbsPath>,
    /// The directory that holds every project's sessions, and the project
    /// whose own sessions may be read.
    sessions: Option<(AbsPath, ProjectKey)>,
}

impl Protected {
    /// Nothing protected: for tests, and for a harness with no directories.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Closes a tree to reading and writing: the configuration directory,
    /// where credentials are, and the state directory, where trust and
    /// stored approvals are.
    #[must_use]
    pub fn closed(mut self, tree: AbsPath) -> Self {
        self.closed.push(tree);
        self
    }

    /// Closes a tree to writing: the data directory.
    #[must_use]
    pub fn unwritable(mut self, tree: AbsPath) -> Self {
        self.read_only.push(tree);
        self
    }

    /// Closes the session logs of every project but this one to reading.
    /// `sessions` is the directory that holds one directory per project.
    #[must_use]
    pub fn other_sessions(mut self, sessions: AbsPath, project: ProjectKey) -> Self {
        self.sessions = Some((sessions, project));
        self
    }

    /// What is protected from reading somewhere below `tree`, if anything
    /// is.
    fn guards_under(&self, tree: &AbsPath) -> Option<&'static str> {
        let below = |protected: &AbsPath| protected != tree && protected.starts_with(tree);
        if self.closed.iter().any(below) {
            return Some(
                "a built-in rule that protects Cortex-Harness's own configuration, credentials, \
                 trust and stored approvals",
            );
        }
        if self
            .sessions
            .as_ref()
            .is_some_and(|(sessions, _)| sessions.starts_with(tree))
        {
            return Some("a built-in rule that protects the session logs of other projects");
        }
        None
    }

    /// What protects `path` from `effect`, if anything does. Compared as
    /// the style compares names: matching too much is the safe side.
    fn guards(&self, effect: &Effect, path: &AbsPath) -> Option<&'static str> {
        if self.closed.iter().any(|tree| path.starts_with(tree)) {
            return Some(
                "a built-in rule that protects Cortex-Harness's own configuration, credentials, \
                 trust and stored approvals",
            );
        }
        if *effect == Effect::Write && self.read_only.iter().any(|tree| path.starts_with(tree)) {
            return Some("a built-in rule that protects Cortex-Harness's own session data");
        }
        if let Some((sessions, project)) = &self.sessions
            && let Some(inside) = path.strip_prefix(sessions)
        {
            let own = inside
                .names()
                .next()
                .is_some_and(|first| first == project.as_str());
            if *effect == Effect::Write || !own {
                return Some(
                    "a built-in rule that protects the session logs of other projects, and every \
                     session log from being changed",
                );
            }
        }
        None
    }
}

/// How a compiled rule matches.
#[derive(Debug, Clone)]
enum Matcher {
    Path { exact: PathGlob, folded: PathGlob },
    Exec(ExecPattern),
    Host(NamePattern),
    Tool(NamePattern),
}

#[derive(Debug, Clone)]
struct Compiled {
    rule: SourcedRule,
    matcher: Matcher,
}

/// What decided a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By<'a> {
    /// A rule somebody wrote.
    Rule(&'a SourcedRule),
    /// A built-in protection, described.
    Builtin(&'static str),
}

/// The strongest rule that matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit<'a> {
    /// What it decides.
    pub action: RuleAction,
    /// What decided.
    pub by: By<'a>,
}

impl Hit<'_> {
    /// The rule in words, for the model and the approval prompt: "the rule
    /// `deny write **/.env*` from your configuration".
    #[must_use]
    pub fn describe(&self) -> String {
        match self.by {
            By::Rule(rule) => format!("the rule `{}` from {}", rule.show(), rule.source.describe()),
            By::Builtin(what) => what.to_owned(),
        }
    }
}

fn strength(action: RuleAction) -> u8 {
    match action {
        RuleAction::Allow => 0,
        RuleAction::Ask => 1,
        RuleAction::Deny => 2,
    }
}

/// The rules in force, compiled.
#[derive(Debug, Clone)]
pub struct RuleSet {
    rules: Vec<Compiled>,
    withheld: Vec<SourcedRule>,
    protected: Protected,
}

impl RuleSet {
    /// Compiles rules.
    ///
    /// In a project that is not trusted, an allow from the project or from
    /// its learned commands is set aside and not compiled.
    ///
    /// # Errors
    ///
    /// Every rule whose pattern cannot be read, or whose effect this build
    /// does not know. The whole set is refused: a rule that is skipped
    /// could be the one that denies.
    pub fn compile(
        rules: Vec<SourcedRule>,
        context: RuleContext<'_>,
        protected: Protected,
    ) -> Result<Self, Vec<RuleError>> {
        let anchors = Anchors {
            workspace: context.workspace,
            home: context.home,
        };
        let mut compiled = Vec::with_capacity(rules.len());
        let mut withheld = Vec::new();
        let mut errors = Vec::new();
        for rule in rules {
            if rule.rule.action == RuleAction::Allow
                && rule.source.allow_needs_trust()
                && !context.trusted
            {
                withheld.push(rule);
                continue;
            }
            let pattern = rule.rule.pattern.as_str();
            let matcher = match &rule.rule.effect {
                Effect::Read | Effect::Write => PathGlob::compile(pattern, anchors, Case::Exact)
                    .and_then(|exact| {
                        let folded = PathGlob::compile(pattern, anchors, Case::Folded)?;
                        // What grants is matched narrowly: an allow that
                        // starts anywhere (`**/…`) is an allow in the
                        // workspace.
                        Ok(if rule.rule.action == RuleAction::Allow {
                            Matcher::Path {
                                exact: exact.kept_within(context.workspace),
                                folded: folded.kept_within(context.workspace),
                            }
                        } else {
                            Matcher::Path { exact, folded }
                        })
                    })
                    .map_err(|error| error.problem),
                Effect::Exec => ExecPattern::parse(pattern)
                    .map(Matcher::Exec)
                    .map_err(|error| error.problem),
                Effect::Network => NamePattern::parse(&pattern.to_ascii_lowercase())
                    .map(Matcher::Host)
                    .map_err(|error| error.problem),
                Effect::Ext => NamePattern::parse(pattern)
                    .map(Matcher::Tool)
                    .map_err(|error| error.problem),
                Effect::Unknown(effect) => {
                    Err(format!("this version does not know the effect `{effect}`"))
                }
            };
            match matcher {
                Ok(matcher) => compiled.push(Compiled { rule, matcher }),
                Err(problem) => errors.push(RuleError { rule, problem }),
            }
        }
        if errors.is_empty() {
            Ok(Self {
                rules: compiled,
                withheld,
                protected,
            })
        } else {
            Err(errors)
        }
    }

    /// A set with no rules and nothing protected.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            rules: Vec::new(),
            withheld: Vec::new(),
            protected: Protected::none(),
        }
    }

    /// The allow rules set aside because the project is not trusted.
    #[must_use]
    pub fn withheld(&self) -> &[SourcedRule] {
        &self.withheld
    }

    /// The rules in force, as written.
    pub fn rules(&self) -> impl Iterator<Item = &SourcedRule> {
        self.rules.iter().map(|compiled| &compiled.rule)
    }

    /// The strongest of the rules that `matches` picks.
    fn strongest<'a>(&'a self, matches: impl Fn(&'a Compiled) -> bool) -> Option<Hit<'a>> {
        self.rules
            .iter()
            .filter(|compiled| matches(compiled))
            .max_by_key(|compiled| strength(compiled.rule.rule.action))
            .map(|compiled| Hit {
                action: compiled.rule.rule.action,
                by: By::Rule(&compiled.rule),
            })
    }

    /// What the rules say of reading or writing `path`, which must be a
    /// resolved path. `effect` is `read` or `write`.
    #[must_use]
    pub fn path(&self, effect: &Effect, path: &AbsPath) -> Option<Hit<'_>> {
        if let Some(what) = self.protected.guards(effect, path) {
            return Some(Hit {
                action: RuleAction::Deny,
                by: By::Builtin(what),
            });
        }
        self.strongest(|compiled| {
            compiled.rule.rule.effect == *effect
                && match &compiled.matcher {
                    Matcher::Path { exact, folded } => {
                        if compiled.rule.rule.action == RuleAction::Allow {
                            exact.matches(path)
                        } else {
                            folded.matches(path) || exact.matches(path)
                        }
                    }
                    _ => false,
                }
        })
    }

    /// What holds back reading something below the directory `tree`, if
    /// anything does: a `deny` or `ask` rule on reading that can match a
    /// path in the tree, or a protected tree of the harness's own inside
    /// it. For a command that reads every file of a tree and names none.
    ///
    /// Errs towards an answer: a rule with `**` is taken to reach
    /// everywhere below where the `**` stands.
    #[must_use]
    pub fn read_held_back_under(&self, tree: &AbsPath) -> Option<Hit<'_>> {
        if let Some(what) = self.protected.guards_under(tree) {
            return Some(Hit {
                action: RuleAction::Deny,
                by: By::Builtin(what),
            });
        }
        self.strongest(|compiled| {
            compiled.rule.rule.effect == Effect::Read
                && compiled.rule.rule.action != RuleAction::Allow
                && match &compiled.matcher {
                    Matcher::Path { exact, folded } => {
                        folded.reaches_under(tree) || exact.reaches_under(tree)
                    }
                    _ => false,
                }
        })
    }

    /// The protection of the harness's own files somewhere below the
    /// directory `tree`, if any: what a pattern that starts there could
    /// reach without naming it.
    #[must_use]
    pub fn protected_under(&self, tree: &AbsPath) -> Option<Hit<'_>> {
        self.protected.guards_under(tree).map(|what| Hit {
            action: RuleAction::Deny,
            by: By::Builtin(what),
        })
    }

    /// The strongest `deny` or `ask` rule on a command line, if any. The
    /// raw line is matched loosely, and each analysed part strictly.
    #[must_use]
    pub fn exec_restriction(
        &self,
        line: &str,
        parts: Option<&[Part]>,
        syntax: ShellSyntax,
    ) -> Option<Hit<'_>> {
        self.strongest(|compiled| {
            compiled.rule.rule.action != RuleAction::Allow
                && match &compiled.matcher {
                    Matcher::Exec(pattern) => {
                        pattern.restricts(line)
                            || parts.is_some_and(|parts| {
                                parts.iter().any(|part| pattern.allows(part, syntax))
                            })
                    }
                    _ => false,
                }
        })
    }

    /// The allow rule for one simple command of an analysed line, if any.
    #[must_use]
    pub fn exec_allowance(&self, part: &Part, syntax: ShellSyntax) -> Option<&SourcedRule> {
        self.rules
            .iter()
            .find(|compiled| {
                compiled.rule.rule.action == RuleAction::Allow
                    && matches!(&compiled.matcher, Matcher::Exec(pattern) if pattern.allows(part, syntax))
            })
            .map(|compiled| &compiled.rule)
    }

    /// What the rules say of reaching `host`. `None` is a call that cannot
    /// say which host it will reach: every rule that holds back applies to
    /// it, and only an allow for every host (`*`) allows it.
    #[must_use]
    pub fn network(&self, host: Option<&str>) -> Option<Hit<'_>> {
        let host = host.map(|host| host.trim_end_matches('.').to_ascii_lowercase());
        self.strongest(|compiled| match (&compiled.matcher, &host) {
            (Matcher::Host(pattern), Some(host)) => pattern.matches(host),
            (Matcher::Host(pattern), None) => {
                compiled.rule.rule.action != RuleAction::Allow || pattern.is_any()
            }
            _ => false,
        })
    }

    /// What the rules say of calling the extension or MCP tool `tool`, by
    /// its full name, `source/tool`.
    #[must_use]
    pub fn ext(&self, tool: &str) -> Option<Hit<'_>> {
        self.strongest(
            |compiled| matches!(&compiled.matcher, Matcher::Tool(pattern) if pattern.matches(tool)),
        )
    }
}

#[cfg(test)]
mod tests;
