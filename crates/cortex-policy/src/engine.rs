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
//! - **`cd ./dir && …` is followed,** with `..` taken as the shell takes
//!   it. The parts after it are judged from the new directory, and one that
//!   may write is judged as a command run there. A `cd` whose effect cannot
//!   be known (in a pipeline, before `;` or `||`, or to a plain name, which
//!   `CDPATH` could send anywhere) takes the line off every safe list; what
//!   comes after it is still checked for what it could reach from anywhere.
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
use crate::command::pattern::{raw_cuts, raw_words, words_of};
use crate::command::{Analysis, Joiner, Part, Redirect, ShellSyntax, Word, analyse, table};
use crate::expansion::{NamePattern, has_wildcards};
use crate::lease::{StandingTerms, standing_lease};
use crate::level::{self, Decider, LevelError, Matter, SandboxState};
use crate::rules::{By, Hit, Protected, RuleContext, RuleError, RuleSet, RuleSource, SourcedRule};
use crate::secrets::{looks_secret, secret_patterns};

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
    /// Whether "always allow in this project" can be kept: the engine was
    /// given a store ([`Parts::store`]).
    pub storable: bool,
}

impl Question {
    /// The answers on offer. "Always allow in this project" only where it
    /// would be kept: offered with nowhere to keep it, it would allow the
    /// call once and save nothing, while the person believed it saved.
    #[must_use]
    pub fn choices(&self) -> Vec<ApprovalChoice> {
        let mut choices = vec![ApprovalChoice::AllowOnce];
        if !self.lifts.is_empty() {
            choices.push(ApprovalChoice::AllowSession);
            if self.storable {
                choices.push(ApprovalChoice::AllowProject);
            }
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

    /// The names in the directory `dir`, a real location, up to `most` of
    /// them: what a pattern a shell expands there could match. The default
    /// is [`Listing::Unknown`], a `Locate` that does not look, and then a
    /// pattern is judged by its text alone.
    fn list(&self, dir: &AbsPath, most: usize) -> Listing {
        let _ = (dir, most);
        Listing::Unknown
    }

    /// Whether `path`, a real location, is a directory: `None` where that
    /// is not known. The default does not look, and a `cd` is then taken
    /// as the logical reading has it.
    fn is_directory(&self, path: &AbsPath) -> Option<bool> {
        let _ = path;
        None
    }
}

/// What a directory holds, for expanding a pattern against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// The names in it, `.` and `..` left out, each with whether it is a
    /// link, which is followed before what it leads to is judged. None, for
    /// a directory that is not there, or for something that is not a
    /// directory.
    Names(Vec<(String, bool)>),
    /// More names than were asked for.
    TooMany,
    /// It could not be listed, or a name in it is not one the harness can
    /// judge.
    Unknown,
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
        // `~\x` is a name of its own outside Windows, where `\` is no
        // separator (found by the final review of the fixes to step 12).
        let is_home = written == "~"
            || written.starts_with("~/")
            || (cfg!(windows) && written.starts_with("~\\"));
        let resolved = if is_home || starts_elsewhere(written, PathStyle::NATIVE) {
            self.resolve(written)
        } else {
            let separator = if cfg!(windows) { "\\" } else { "/" };
            self.resolve(&format!("{}{separator}{written}", cwd.to_native_string()))
        };
        resolved.ok().map(|resolved| resolved.abs().clone())
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "one look at one path, where a decision about a `cd` needs it"
    )]
    fn is_directory(&self, path: &AbsPath) -> Option<bool> {
        match std::fs::metadata(path.to_fs_path()) {
            Ok(metadata) => Some(metadata.is_dir()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "a listing of one directory, bounded, where a decision about a pattern needs it"
    )]
    fn list(&self, dir: &AbsPath, most: usize) -> Listing {
        let entries = match std::fs::read_dir(dir.to_fs_path()) {
            Ok(entries) => entries,
            // Nothing there to match.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Listing::Names(Vec::new());
            }
            Err(_) => return Listing::Unknown,
        };
        let mut names = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                return Listing::Unknown;
            };
            if names.len() == most {
                return Listing::TooMany;
            }
            // A file type that cannot be told is taken for a link, and
            // followed.
            let link = entry.file_type().map_or(true, |kind| kind.is_symlink());
            match entry.file_name().into_string() {
                Ok(name) => names.push((name, link)),
                Err(_) => return Listing::Unknown,
            }
        }
        Listing::Names(names)
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
    /// Whether the project's allow rules apply: the user has trusted the
    /// project, and its rules are the ones they approved
    /// ([`crate::trust::ProjectTrust::rules_approved`]). Any change to the
    /// project's rules sets this to `false` until the user approves them
    /// again.
    pub trusted: bool,
    /// Whether the user has trusted the project at all
    /// ([`crate::trust::ProjectTrust::is_trusted`]): whether git commands
    /// that read the repository's own configuration (`git status`, `git
    /// diff`) may be on the table of commands that only read. A project
    /// that adds a rule that holds back keeps them, though its allow rules
    /// wait for the user.
    pub repository_trusted: bool,
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
    repository_trusted: bool,
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

/// Where a part of a command line runs.
#[derive(Debug, Clone)]
struct Directory {
    /// Where the shell believes it is: what `cd ..` moves up from.
    logical: AbsPath,
    /// Where that really is, links followed: what a path a command names
    /// is found from.
    real: AbsPath,
}

/// What the other parts of a line could have done by the time a part's
/// words are expanded: written, so that names a listing taken now cannot
/// see may be there; or made a link, so that a path may lead anywhere. A
/// part after one of those, or beside one in a pipeline, which runs at the
/// same time, is affected.
#[derive(Debug)]
struct Around {
    /// For each part: whether it may write.
    writes: Vec<bool>,
    /// For each part: whether it makes a link.
    links: Vec<bool>,
    /// For each part: the index of the first part of its pipeline.
    pipeline: Vec<usize>,
}

impl Around {
    fn of(engine: &Engine, parts: &[Part]) -> Self {
        let mut pipeline = Vec::with_capacity(parts.len());
        for (index, part) in parts.iter().enumerate() {
            let start = if part.joiner == Joiner::Pipe && index > 0 {
                pipeline[index - 1]
            } else {
                index
            };
            pipeline.push(start);
        }
        Self {
            writes: parts
                .iter()
                .map(|part| {
                    part.redirects
                        .iter()
                        .any(|redirect| matches!(redirect, Redirect::Write { .. }))
                        || (!is_change_of_directory(part, engine.shell)
                            && table::read_only(part, engine.shell, engine.repository_trusted)
                                .is_none())
                })
                .collect(),
            links: parts.iter().map(makes_a_link).collect(),
            pipeline,
        }
    }

    /// The parts that could act before the words of part `index` are
    /// expanded: every one before it, and the rest of its pipeline.
    fn others(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        (0..self.writes.len()).filter(move |&other| {
            other != index && (other < index || self.pipeline[other] == self.pipeline[index])
        })
    }

    /// Whether a pattern of part `index` may be expanded against the
    /// directories as they are now.
    fn may_list(&self, index: usize) -> bool {
        self.others(index).all(|other| !self.writes[other])
    }

    /// Whether a part that makes a link runs before part `index`, or beside
    /// it.
    fn after_a_link(&self, index: usize) -> bool {
        self.others(index).any(|other| self.links[other])
    }
}

/// Whether a part makes a link: `ln`, `link`, `mklink`, or `cp` told to make
/// links instead of copies.
fn makes_a_link(part: &Part) -> bool {
    let program = part.program();
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    match name {
        "ln" | "link" | "mklink" => true,
        "cp" => part.arguments().iter().any(|word| {
            let text = word.text.as_str();
            matches!(text, "--symbolic-link" | "--link")
                || (text.starts_with('-') && !text.starts_with("--") && text.contains(['s', 'l']))
        }),
        _ => false,
    }
}

/// Where the parts after a `cd` the analysis could not follow may be
/// running. Any `cd` may have failed, and then the next ran from where the
/// one before left the shell. So each later `cd` is applied from every
/// place so far, and the places only grow: the directories the line was in
/// before (`cd -` goes back to one of them), the workspace root, and where
/// each `cd` since would lead with no `CDPATH`. Checking a path from every
/// one of them can only ask once too often. Past [`MOST_PLACES`] the parts
/// after are not placed at all, and are asked about. Found by the second
/// independent review of the fixes to step 12: `cd nope; cd
/// .github/workflows; cp x ci.yml` wrote the workflow unasked.
#[derive(Debug)]
struct Lost {
    /// Where the shell may believe it is, and where that really is.
    places: Vec<Directory>,
    /// Where a `cd` to a pattern may have led: the directory it started
    /// from, and the pattern with every `cd` after it joined on (`.gi?`,
    /// then `.gi?/hooks`). A path a later part names is tried under each.
    patterns: Vec<(AbsPath, String)>,
    /// Whether there were more places than are kept.
    overflowed: bool,
}

/// The most places and patterns a line that lost track of its directory
/// is followed through.
const MOST_PLACES: usize = 32;

impl Lost {
    /// The places after the `cd` of `part`, for a line that has been in the
    /// directories `visited`.
    fn after(engine: &Engine, visited: &[Directory], part: &Part) -> Self {
        let mut lost = Self {
            places: Vec::new(),
            patterns: Vec::new(),
            overflowed: false,
        };
        for directory in visited {
            lost.add(directory.clone());
        }
        // `cd -` before any other `cd` of the line goes to the directory
        // the shell was given in `OLDPWD`: anywhere (found by the final
        // review of the fixes to step 12).
        if visited.len() == 1 && matches!(cd_operand(part), CdOperand::Back) {
            lost.overflowed = true;
        }
        lost.add(Directory {
            logical: engine.workspace.clone(),
            real: engine.workspace.clone(),
        });
        lost.moved_by(engine, part);
        lost
    }

    fn add(&mut self, directory: Directory) {
        if self
            .places
            .iter()
            .any(|place| place.logical == directory.logical && place.real == directory.real)
        {
            return;
        }
        if self.places.len() + self.patterns.len() >= MOST_PLACES {
            self.overflowed = true;
            return;
        }
        self.places.push(directory);
    }

    fn add_pattern(&mut self, base: AbsPath, pattern: String) {
        if self
            .patterns
            .iter()
            .any(|(held, text)| *held == base && *text == pattern)
        {
            return;
        }
        if self.places.len() + self.patterns.len() >= MOST_PLACES {
            self.overflowed = true;
            return;
        }
        self.patterns.push((base, pattern));
    }

    /// The real directories, each once.
    fn reals(&self) -> Vec<AbsPath> {
        let mut reals: Vec<AbsPath> = Vec::new();
        for place in &self.places {
            if !reals.contains(&place.real) {
                reals.push(place.real.clone());
            }
        }
        reals
    }

    /// Adds where the `cd` of `part` would lead, from every place so far.
    fn moved_by(&mut self, engine: &Engine, part: &Part) {
        let target = match cd_operand(part) {
            CdOperand::Directory { target, .. } => target,
            CdOperand::Home => {
                if let Some(home) = &engine.home
                    && let Some(real) = engine.locate.locate(home, home.as_str())
                {
                    self.add(Directory {
                        logical: home.clone(),
                        real,
                    });
                }
                return;
            }
            // Back to a directory the line was in: a place already.
            CdOperand::Back => return,
            // Anywhere: nothing after it can be placed.
            CdOperand::Unknown => {
                self.overflowed = true;
                return;
            }
        };
        let starts_anywhere =
            target.home || starts_elsewhere(&target.text, engine.workspace.style());
        let from = self.places.clone();
        let earlier = self.patterns.clone();
        if !target.is_fixed() && has_wildcards(&target.text) {
            // A pattern: it may lead to any directory it matches.
            if starts_anywhere {
                self.add_pattern(engine.workspace.clone(), target.text.clone());
            } else {
                for place in &from {
                    self.add_pattern(place.real.clone(), target.text.clone());
                }
            }
        } else {
            for place in &from {
                if let Some(logical) = engine.cd_target(&place.logical, target, false)
                    && let Some(real) = engine.locate.locate(&logical, logical.as_str())
                {
                    self.add(Directory { logical, real });
                }
                // `cd -P` takes `..` through links, and so does bash when
                // the logical directory is not there: where the file system
                // leads from the real directory is a place too.
                if let Some(real) = engine.locate.locate(&place.real, &target.text) {
                    self.add(Directory {
                        logical: real.clone(),
                        real,
                    });
                }
            }
        }
        // After a pattern, on from wherever it led.
        if !starts_anywhere {
            for (base, pattern) in earlier {
                self.add_pattern(base, format!("{pattern}/{}", target.text));
            }
        }
    }
}

/// How a part uses what a path or a pattern it gives leads to.
#[derive(Debug, Clone, Copy)]
struct How {
    /// It only reads it.
    reads: bool,
    /// It writes it for certain, as a redirection does: a match outside the
    /// scope is asked about, as a file named outright is.
    scope: bool,
    /// Its directory may be listed to expand a pattern: nothing earlier in
    /// the line, or beside it in a pipeline, can have made names there.
    listing: bool,
}

/// The most names one directory is listed for, to expand a pattern against
/// it, and the most a pattern's matches are judged. Enough for a directory
/// of sources or build output (`rm -rf dist/*`), and bounded so that a
/// decision stays within a few milliseconds on a local disk (6.7 ms for a
/// thousand names, measured in the fixes to step 12). Past it the pattern
/// is asked about.
const MOST_NAMES_LISTED: usize = 1000;

/// The most directories listed for one pattern: `*/*/x` lists the base
/// and then every directory its first name matched.
const MOST_DIRECTORIES_LISTED: usize = 64;

/// The most work one command's decision does: names listed, paths found
/// and patterns read. A line of normal size does a few dozen; a long
/// word of option letters with a pattern in it (`-aaaa…*`) would list the
/// directory once for each letter. Past it, the command is asked about
/// (found by the final review of the fixes to step 12).
const MOST_WORK: usize = 10_000;

thread_local! {
    /// The work done so far for the command being decided on this thread.
    /// A decision runs on one thread from start to end.
    static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts `units` of work for the command being decided: `false` once it
/// is more than [`MOST_WORK`].
fn spend(units: usize) -> bool {
    WORK.with(|work| {
        let done = work.get().saturating_add(units);
        work.set(done);
        done <= MOST_WORK
    })
}

/// Whether a word may be a command line of its own: it has a space or one
/// of a shell's operators in it.
fn may_be_a_line(text: &str) -> bool {
    text.contains(|character: char| {
        character.is_whitespace() || matches!(character, '<' | '>' | '|' | ';' | '&' | '`')
    }) || text.contains("$(")
}

/// What a pattern expands to, against the directories as they are now.
#[derive(Debug)]
enum Expansion {
    /// The paths it matches, each with whether a link may be in it that
    /// is still to be followed.
    Matches(Vec<(AbsPath, bool)>),
    /// More than [`MOST_NAMES_LISTED`] names, or directories.
    TooMany,
    /// A directory could not be listed: the pattern is judged by its text.
    Unknown,
}

/// What a `cd` is given, past its options.
#[derive(Debug)]
enum CdOperand<'a> {
    /// No directory: the home directory.
    Home,
    /// One directory, and whether `-P` asks for `..` to be taken through
    /// links.
    Directory { target: &'a Word, physical: bool },
    /// `-`: back to the directory before.
    Back,
    /// Anything else: an option this does not know, several directories.
    Unknown,
}

/// Reads a `cd`'s arguments the way the shells do: `-L`, `-P`, `-e` and
/// `-@` (alone or together, the last of `-L` and `-P` counting), then `--`
/// or the directory. Found by the independent review of the fixes to step
/// 12: `cd -- .git/hooks && …` was not followed, and neither was the
/// directory it led to looked in.
fn cd_operand(part: &Part) -> CdOperand<'_> {
    let mut physical = false;
    let mut words = part.arguments().iter();
    let mut operands: Vec<&Word> = Vec::new();
    while let Some(word) = words.next() {
        let text = word.text.as_str();
        if text == "--" {
            operands.extend(words.by_ref());
            break;
        }
        if text.len() > 1 && text.starts_with('-') {
            let options = &text[1..];
            if !word.is_fixed()
                || !options
                    .chars()
                    .all(|option| matches!(option, 'L' | 'P' | 'e' | '@'))
            {
                return CdOperand::Unknown;
            }
            for option in options.chars() {
                match option {
                    'P' => physical = true,
                    'L' => physical = false,
                    _ => {}
                }
            }
            continue;
        }
        operands.push(word);
        operands.extend(words.by_ref());
        break;
    }
    match operands.as_slice() {
        [] => CdOperand::Home,
        [target] if target.text == "-" => CdOperand::Back,
        [target] => CdOperand::Directory { target, physical },
        _ => CdOperand::Unknown,
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
            repository_trusted: parts.repository_trusted,
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
        combine(findings, read_only, self.store.is_some())
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
            || is_build_configuration(path)
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
                // That host and no other: a host is matched as a pattern, so
                // `*` must not become "every host", and `[::1]` must match
                // itself (found by the review of step 12).
                lift: host.map(|host| Rule {
                    effect: Effect::Network,
                    pattern: literal_name(&host.trim_end_matches('.').to_ascii_lowercase()),
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
        WORK.with(|work| work.set(0));
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
                findings.extend(self.judge_unmodelled(rules, command, std::slice::from_ref(cwd)));
                return (findings, false);
            }
        };

        // Where each part runs: the directory the shell believes it is in,
        // which is what a later `cd` moves from, and where that really is,
        // which is what a path a part names is found from. Once a `cd`
        // cannot be followed, the places a part may be running in instead.
        let mut directory = Directory {
            logical: cwd.clone(),
            real: cwd.clone(),
        };
        // Every directory the line has been in, for `cd -` after a `cd` that
        // could not be followed.
        let mut visited = vec![directory.clone()];
        let mut lost: Option<Lost> = None;
        let mut only_reads = true;
        let around = Around::of(self, parts);
        for (index, part) in parts.iter().enumerate() {
            let changes_directory = is_change_of_directory(part, self.shell);
            let later = parts.get(index + 1..).unwrap_or(&[]);
            let listing = around.may_list(index);
            let names_something = !part.arguments().is_empty()
                || part
                    .redirects
                    .iter()
                    .any(|redirect| !matches!(redirect, Redirect::Duplicate));
            if around.after_a_link(index)
                && names_something
                && level::decider(self.level(), Matter::WriteBeyond) != Decider::Holder
            {
                findings.push(Finding::ask(format!(
                    "`{}` runs after a part of the line that makes a link, which can give any \
                     path another name, so what it names cannot be checked.",
                    words_of(part).join(" ")
                )));
            }
            if let Some(lost) = &mut lost {
                // A `cd`'s own redirections are opened before it moves, and
                // its target is not itself a path it reads or writes.
                findings.extend(self.judge_lost(rules, part, lost, !changes_directory, listing));
                if changes_directory {
                    lost.moved_by(self, part);
                }
                continue;
            }
            if changes_directory && part.redirects.is_empty() {
                // A last `cd` changes nothing that is checked. Any other is
                // followed where its effect is certain.
                if !later.is_empty() {
                    match self.follow_cd(part, later, &directory) {
                        Some(next) => {
                            directory = next;
                            visited.push(directory.clone());
                        }
                        None => {
                            only_reads = false;
                            findings.push(self.lost_finding());
                            lost = Some(Lost::after(self, &visited, part));
                        }
                    }
                }
                continue;
            }
            let here = directory.real.clone();
            if changes_directory && !later.is_empty() {
                // A `cd` with a redirection: judged as any command, for
                // what it writes, below, and not followed.
                only_reads = false;
                findings.push(self.lost_finding());
                lost = Some(Lost::after(self, &visited, part));
            }
            let (standing, found) = self.judge_part(rules, part, &here, listing);
            findings.extend(found);
            only_reads &= standing == Standing::ReadOnly;
            // A command that may write, run after a `cd`, writes where it
            // runs: judged as the same command would be with the tool's
            // working directory there. (One that only reads had its
            // directory judged as a read.)
            if standing != Standing::ReadOnly && here != *cwd {
                findings.push(self.judge_command_write(rules, &here));
            }
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
    /// - **Its target says where it starts:** `/`, `~`, `./` or `../`, or it
    ///   is `.` or `..`. Any other name is looked for in each directory of
    ///   the shell's `CDPATH` first, which the environment can set, and so
    ///   can zsh's `cdpath` in a `.zshenv` that `zsh -c` reads. Such a `cd`
    ///   could lead anywhere.
    ///
    /// **`..` is taken logically, as a POSIX shell's `cd` takes it:** it
    /// removes the name before it from the directory the shell believes it
    /// is in, whatever that name links to. Through a link `a` to `b/c`,
    /// `cd a/../..` goes up two from where the shell is, not one from the
    /// link's target (found by the review of step 12). Only the directory
    /// reached is then followed through the file system. A path a later
    /// command names is found as the kernel finds it, from that real
    /// directory. **Where the logical directory is not there and the
    /// kernel's reading differs,** the `cd` is not followed, and both are
    /// places: bash then takes the target through links (found by the final
    /// review of the fixes to step 12: `cd ./l/../workflows` with a link `l`
    /// to `.github/x`).
    ///
    /// `directory` is where the shell believes it is, and where that really
    /// is. The answer is the same after the `cd`.
    fn follow_cd(&self, part: &Part, later: &[Part], directory: &Directory) -> Option<Directory> {
        // `cd -- dir` and `cd -L dir` are `cd dir`. `cd -P dir` takes `..`
        // through links, and is not followed.
        let CdOperand::Directory {
            target,
            physical: false,
        } = cd_operand(part)
        else {
            return None;
        };
        let certain = self.shell == ShellSyntax::Posix
            && !matches!(part.joiner, Joiner::Pipe | Joiner::Or)
            && target.is_fixed()
            && later.first().is_none_or(|next| next.joiner == Joiner::And)
            && later
                .iter()
                .all(|part| matches!(part.joiner, Joiner::And | Joiner::Pipe));
        if !certain {
            return None;
        }
        let moved = self.cd_target(&directory.logical, target, true)?;
        let real = self.locate.locate(&moved, moved.as_str())?;
        if self.locate.is_directory(&real) == Some(false)
            && self.locate.locate(&directory.real, &target.text).as_ref() != Some(&real)
        {
            return None;
        }
        Some(Directory {
            logical: moved,
            real,
        })
    }

    /// Where `cd target` from `logical` leads, as the shell believes it:
    /// `..` taken as text. With `says_where`, only a target that says where
    /// it starts is read (see [`follow_cd`](Self::follow_cd)); without, a
    /// plain name is read as under `logical`, which is where it leads when
    /// no `CDPATH` sends it elsewhere.
    fn cd_target(&self, logical: &AbsPath, target: &Word, says_where: bool) -> Option<AbsPath> {
        let text = target.text.as_str();
        let style = logical.style();
        let under = |base: &AbsPath, rest: &str| {
            let base = base.as_str();
            let joined = if base.ends_with('/') {
                format!("{base}{rest}")
            } else {
                format!("{base}/{rest}")
            };
            AbsPath::parse(&joined, style).ok()
        };
        if target.home {
            let home = self.home.as_ref()?;
            return match text.strip_prefix("~/") {
                Some(rest) => under(home, rest),
                None if text == "~" => Some(home.clone()),
                None => None,
            };
        }
        if text.starts_with('/') {
            return AbsPath::parse(text, style).ok();
        }
        let plain = matches!(text, "." | "..") || text.starts_with("./") || text.starts_with("../");
        if says_where && !plain {
            return None;
        }
        under(logical, text)
    }

    /// The finding for a line that changes directory where the analysis
    /// cannot follow: no safe list applies, and the level decides.
    fn lost_finding(&self) -> Finding {
        self.other_command(
            "The command changes directory in a way the analysis cannot follow, so what comes \
             after it is checked only for what it could reach from anywhere: protected paths, \
             files that look like secrets, and the harness's own files."
                .to_owned(),
        )
    }

    /// Judges a part that runs after a `cd` the analysis could not follow.
    /// Where it runs is not known, so it is on no safe list, and the level
    /// decides it (the caller has said so once). What does not depend on
    /// where it runs is still judged: a path that starts at a root or at
    /// `~` as it would be anywhere, `git config`, and every other path
    /// the part names or redirects to, tried from each place it may be
    /// running in, for a protected path, a file that looks like a secret,
    /// or a path a rule holds back.
    fn judge_lost(
        &self,
        rules: &RuleSet,
        part: &Part,
        lost: &Lost,
        arguments: bool,
        listing: bool,
    ) -> Vec<Finding> {
        let shown = words_of(part).join(" ");
        let mut findings = Vec::new();
        if lost.overflowed && level::decider(self.level(), Matter::WriteBeyond) != Decider::Holder {
            findings.push(Finding::ask(format!(
                "`{shown}` runs after more changes of directory than the analysis follows, so \
                 what it names cannot be placed."
            )));
        }
        let places = lost.reals();
        let mut relative = Vec::new();
        for redirect in &part.redirects {
            let (target, effect) = match redirect {
                Redirect::Duplicate => continue,
                Redirect::Read(target) => (target, Effect::Read),
                Redirect::Write { target, .. } => (target, Effect::Write),
            };
            if target.text == "/dev/null" && self.shell == ShellSyntax::Posix {
                continue;
            }
            let reads = effect == Effect::Read;
            if !target.is_fixed() && has_wildcards(&target.text) {
                relative.push((target.text.clone(), true, reads));
                if !reads && let Some(found) = self.pattern_outside(&target.text, &places, &shown) {
                    findings.push(found);
                }
            } else if target.home || starts_elsewhere(&target.text, self.workspace.style()) {
                match self.locate.locate(&self.workspace, &target.text) {
                    Some(path) => {
                        findings.push(self.judge_path(rules, &effect, &path, Some(&shown)));
                    }
                    None => findings.push(Finding::ask(format!(
                        "`{shown}` redirects to or from `{}`, which is not a path that can be \
                         checked.",
                        target.text
                    ))),
                }
            } else {
                for text in self.texts_of(target) {
                    relative.push((text, false, reads));
                }
            }
        }
        for (text, pattern, reads) in &relative {
            let (text, pattern, reads) = (text.as_str(), *pattern, *reads);
            let how = How {
                reads,
                scope: !reads,
                listing,
            };
            let found = if pattern {
                self.judge_pattern(rules, text, &places, &shown, how)
            } else {
                self.judge_name(rules, text, &places, &shown, reads)
            };
            if let Some(found) = found.or_else(|| self.judge_under(rules, text, lost, &shown, how))
            {
                findings.push(found);
            }
        }
        if arguments {
            let how = How {
                reads: self.only_reads_but_for_patterns(part),
                scope: false,
                listing,
            };
            findings.extend(self.judge_named(rules, part, &places, Some(lost), &shown, how));
        }
        findings
    }

    /// Whether a part would be on the table of commands that only read but
    /// for the patterns among its words: what it names it only reads.
    fn only_reads_but_for_patterns(&self, part: &Part) -> bool {
        let mut fixed = part.clone();
        for word in &mut fixed.words {
            word.glob = false;
        }
        part.redirects
            .iter()
            .all(|redirect| !matches!(redirect, Redirect::Write { .. }))
            && table::read_only(&fixed, self.shell, self.repository_trusted).is_some()
    }

    /// Judges a path a part names, after a `cd` to a pattern, as the pattern
    /// joined to it: `hooks/pre-commit` after `cd .gi?` is
    /// `.gi?/hooks/pre-commit`. A path that starts at a root or at `~` does
    /// not depend on where the part runs.
    fn judge_under(
        &self,
        rules: &RuleSet,
        text: &str,
        lost: &Lost,
        shown: &str,
        how: How,
    ) -> Option<Finding> {
        if text.starts_with('~') || starts_elsewhere(text, self.workspace.style()) {
            return None;
        }
        lost.patterns.iter().find_map(|(base, pattern)| {
            self.judge_pattern(
                rules,
                &format!("{pattern}/{text}"),
                std::slice::from_ref(base),
                shown,
                how,
            )
        })
    }

    /// Judges the paths among a command's arguments that a rule holds back,
    /// that are protected or that look like secrets, and `git config`: the
    /// checks that hold whatever allows the command. Each path is tried
    /// from every place in `places`.
    ///
    /// A word the shell expands as a pattern names every path it could
    /// match ([`judge_pattern`](Self::judge_pattern)): in PowerShell, whose
    /// own commands expand a pattern, quoted or not. After a `cd` to a
    /// pattern, a path is also tried under the pattern (`lost`). With
    /// `reads`, the command only reads what it names, and a path is judged
    /// as a read.
    ///
    /// A `~` is the home directory only where the shell takes it as one: at
    /// the start of the word, unquoted. Anywhere else it is a name, and the
    /// text is tried as one too (`"~/../.github/workflows/ci.yml"` is under
    /// a folder named `~` in the workspace). In PowerShell, whose own
    /// commands read a quoted `~` as home, both are tried.
    fn judge_named(
        &self,
        rules: &RuleSet,
        part: &Part,
        places: &[AbsPath],
        lost: Option<&Lost>,
        shown: &str,
        how: How,
    ) -> Vec<Finding> {
        let mut findings = Vec::new();
        for word in part.arguments() {
            let pattern_word = !word.is_fixed() || self.shell == ShellSyntax::PowerShell;
            let found = may_name(&word.text).into_iter().find_map(|text| {
                let home_written = word.home && text.len() == word.text.len();
                let as_name = (text.starts_with('~') && !home_written).then(|| format!("./{text}"));
                let mut texts: Vec<&str> = Vec::new();
                if let Some(as_name) = &as_name {
                    texts.push(as_name);
                }
                if as_name.is_none() || self.shell == ShellSyntax::PowerShell {
                    texts.push(text);
                }
                texts.into_iter().find_map(|text| {
                    let here = if pattern_word && has_wildcards(text) {
                        self.judge_pattern(rules, text, places, shown, how)
                    } else {
                        self.judge_name(rules, text, places, shown, how.reads)
                    };
                    here.or_else(|| {
                        lost.and_then(|lost| self.judge_under(rules, text, lost, shown, how))
                    })
                })
            });
            if let Some(found) = found {
                findings.push(found);
            }
        }
        // A word with a space or a shell's operator in it may be a command
        // line for another program (`sh -c '…'`, `ssh host '…'`,
        // `bash -c 'cat<x>y'`): its words are judged as those of a line the
        // analysis does not model, from every place.
        for word in part.arguments() {
            if may_be_a_line(&word.text) {
                findings.extend(self.judge_unmodelled(rules, &word.text, places));
            }
        }
        // `git config` writes the repository's configuration, which can
        // name programs for git to run.
        if is_git_config(part)
            && level::decider(self.level(), Matter::WriteBeyond) != Decider::Holder
        {
            findings.push(self.beyond(format!(
                "`{shown}` changes git's configuration, which can name programs for git to run."
            )));
        }
        findings
    }

    /// The finding for one text a command gives that may be a path, tried
    /// from each place: `None` when it names nothing held back.
    fn judge_name(
        &self,
        rules: &RuleSet,
        text: &str,
        places: &[AbsPath],
        shown: &str,
        reads: bool,
    ) -> Option<Finding> {
        places.iter().find_map(|place| {
            if !spend(1 + text.len() / 32) {
                return Some(self.too_much(shown));
            }
            let path = self.locate.locate(place, text)?;
            if reads {
                self.judge_read_path(rules, &path, shown)
            } else {
                self.judge_named_path(rules, &path, shown)
            }
        })
    }

    /// The finding for one path a command that only reads names: `None`
    /// when reading it is not held back. A file that looks like a secret is
    /// asked about, as a read of it is.
    fn judge_read_path(&self, rules: &RuleSet, path: &AbsPath, shown: &str) -> Option<Finding> {
        match rules.path(&Effect::Read, path) {
            Some(hit) if hit.action == RuleAction::Allow => None,
            Some(hit) => {
                let reason = format!(
                    "`{shown}` names `{path}`, which is protected by {}.",
                    hit.describe()
                );
                Some(match hit.by {
                    By::Builtin(_) => Finding::Deny {
                        reason,
                        built_in: true,
                    },
                    By::Rule(_) => Finding::ask(reason),
                })
            }
            None => (self.is_secret(path)
                && level::decider(self.level(), Matter::ReadBeyond) != Decider::Holder)
                .then(|| {
                    Finding::ask(format!(
                        "`{shown}` names `{path}`, which looks like a file that holds secrets."
                    ))
                }),
        }
    }

    /// The finding for one path a command names: `None` when nothing holds
    /// it back.
    fn judge_named_path(&self, rules: &RuleSet, path: &AbsPath, shown: &str) -> Option<Finding> {
        if let Some(hit) = held_back(rules, path) {
            let reason = format!(
                "`{shown}` names `{path}`, which is protected by {}.",
                hit.describe()
            );
            return Some(match hit.by {
                By::Builtin(_) => Finding::Deny {
                    reason,
                    built_in: true,
                },
                By::Rule(_) => Finding::ask(reason),
            });
        }
        self.is_sensitive(rules, path).then(|| {
            self.beyond(format!(
                "`{shown}` names `{path}`, which is a protected path or looks like a file that \
                 holds secrets. Whoever can write a protected path can run code later."
            ))
        })
    }

    /// The finding for a pattern a command gives where a path goes, which
    /// the shell replaces with the names of the files it matches: `None`
    /// when nothing it could match is held back. Found during the fixes to
    /// step 12: `cp evil .gi?/hooks/pre-commit` ran unasked at `autonomous`.
    ///
    /// The pattern names every path it could match ([`crate::expansion`]),
    /// tried from each place:
    ///
    /// - **What it starts under** (the part before its first name with a
    ///   wildcard) is judged as a path the command names, so a pattern under
    ///   `.git` or under one of the harness's own directories is held back
    ///   as the directory is. One that starts above one of the harness's own
    ///   directories (`~/.c*/cortex-harness/…`) is asked about.
    /// - **What it could match**, name by name, is asked about at every
    ///   level that asks about protected paths: a name that could be `.git`,
    ///   a last name that could be `HEAD`, a file that looks like a secret,
    ///   or build configuration read from where a build starts, and a path
    ///   that could be one of the protected paths. A name that begins with
    ///   `.` is matched only by a pattern that begins with a `.` written out,
    ///   as in the shells (not in PowerShell, whose own commands expand
    ///   patterns), so `rm *.o` runs and `rm *` is asked about, since it
    ///   could match `HEAD` or `id_rsa`.
    ///
    /// A rule is matched against a pattern only through the directory it
    /// starts under; an allow rule never lifts the question.
    fn judge_pattern(
        &self,
        rules: &RuleSet,
        text: &str,
        places: &[AbsPath],
        shown: &str,
        how: How,
    ) -> Option<Finding> {
        if !spend(1 + text.len() / 32) {
            return Some(self.too_much(shown));
        }
        let reads = how.reads;
        let hidden_rule = self.shell == ShellSyntax::Posix;
        let (_, rest) = self.split_pattern(text)?;
        let names: Vec<&str> = rest
            .split(self.separators())
            .filter(|name| !name.is_empty() && *name != ".")
            .collect();
        let patterns: Vec<NamePattern> =
            names.iter().map(|name| NamePattern::parse(name)).collect();
        let could = |pattern: &NamePattern, name: &str| pattern.could_match_name(name, hidden_rule);
        // dash, and bash before 5.2, match `.` and `..` with `.*` and `.?`:
        // a name that could be `..` could lead anywhere (found by the final
        // review of the fixes to step 12).
        let up = names.contains(&"..")
            || (hidden_rule
                && patterns
                    .iter()
                    .any(|pattern| pattern.could_match_name("..", true)));
        // zsh reads a name `**` as any number of directories, which the
        // listing does not follow.
        let deep = hidden_rule
            && names
                .iter()
                .any(|name| name.len() >= 2 && name.chars().all(|character| character == '*'));
        let flagged = || {
            self.beyond(format!(
                "`{shown}` names `{text}`, a pattern the shell may expand to a protected path or \
                 a file that looks like a secret. Whoever can write a protected path can run \
                 code later."
            ))
        };
        places.iter().find_map(|place| {
            let Some(base) = self.pattern_base(text, place) else {
                // Where the pattern starts cannot be found: it is not a
                // path that can be checked.
                return Some(Finding::ask(format!(
                    "`{shown}` names `{text}`, a pattern that does not start at a path that can be \
                     checked."
                )));
            };
            let found = if reads {
                self.judge_read_path(rules, &base, shown)
            } else {
                self.judge_named_path(rules, &base, shown)
            };
            if found.is_some() {
                return found;
            }
            if deep && let Some(hit) = rules.held_back_under(&base) {
                return Some(Finding::ask(format!(
                    "`{shown}` names `{text}`, a pattern zsh expands at any depth, under `{base}`, \
                     where {} holds a path back.",
                    hit.describe()
                )));
            }
            // What the directory holds now: each match is judged as a name
            // written out, through any link. Where nothing can have made
            // names since, that is all the pattern can match; otherwise its
            // text is judged as well (found by the final review of the fixes
            // to step 12: `mkdir t && rm *.lock` dodged a rule).
            if !deep && !up {
                match self.expand(&base, &names, hidden_rule) {
                    Expansion::Matches(paths) => {
                        let found = paths
                            .iter()
                            .find_map(|path| self.judge_match(rules, path, shown, how));
                        if found.is_some() || how.listing {
                            return found;
                        }
                    }
                    Expansion::TooMany => {
                        return Some(Finding::ask(format!(
                            "`{shown}` names `{text}`, a pattern that matches more than \
                             {MOST_NAMES_LISTED} names, or reaches into more than \
                             {MOST_DIRECTORIES_LISTED} directories: too many to check."
                        )));
                    }
                    Expansion::Unknown => {}
                }
            }
            // Otherwise, by its text: what it could match.
            if let Some(hit) = rules.protected_under(&base) {
                return Some(Finding::ask(format!(
                    "`{shown}` names `{text}`, a pattern the shell may expand to a path protected \
                     by {}.",
                    hit.describe()
                )));
            }
            let secret_names = || {
                std::iter::once("HEAD".to_owned())
                    .chain(secret_patterns())
                    .chain([".yarnrc".to_owned(), ".yarnrc.yml".to_owned()])
            };
            if reads {
                // What a command that only reads could read: a file that
                // looks like a secret is asked about, as a read of it is.
                let could_be_secret = up
                    || patterns.last().is_some_and(|last| {
                        secret_patterns()
                            .any(|name| last.could_match(&NamePattern::parse(&name), hidden_rule))
                    });
                return (could_be_secret
                    && level::decider(self.level(), Matter::ReadBeyond) != Decider::Holder)
                    .then(|| {
                        Finding::ask(format!(
                            "`{shown}` reads `{text}`, a pattern the shell may expand to a file \
                             that looks like a secret."
                        ))
                    });
            }
            if level::decider(self.level(), Matter::WriteBeyond) == Decider::Holder {
                return None;
            }
            // A `..` after a pattern could lead anywhere.
            if up {
                return Some(flagged());
            }
            let last = patterns.last()?;
            let repository = patterns.iter().any(|pattern| could(pattern, ".git"));
            let secret = secret_names()
                .any(|name| last.could_match(&NamePattern::parse(&name), hidden_rule));
            let in_cargo = |at: usize| match at.checked_sub(1) {
                Some(before) => could(&patterns[before], ".cargo"),
                None => base
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case(".cargo")),
            };
            let cargo = patterns.iter().enumerate().any(|(at, pattern)| {
                (could(pattern, "config") || could(pattern, "config.toml")) && in_cargo(at)
            });
            let guarded = self.guarded.iter().any(|guard| {
                guard.strip_prefix(&base).is_some_and(|below| {
                    let below: Vec<&str> = below.names().collect();
                    below.len() <= patterns.len()
                        && below
                            .iter()
                            .zip(&patterns)
                            .all(|(name, pattern)| could(pattern, name))
                })
            });
            (repository || secret || cargo || guarded).then(flagged)
        })
    }

    /// Expands a pattern against the directories as they are now: `names`
    /// below `base`, as a shell would. A name that begins with `.` is
    /// matched only as a shell's pattern matches one. Past the bounds, or
    /// where a directory cannot be listed, it says so.
    fn expand(&self, base: &AbsPath, names: &[&str], hidden_rule: bool) -> Expansion {
        // Each path so far, and whether a link may be in it still to be
        // followed: the base was resolved, and a name a listing gave says
        // whether it is a link, but a name written out may be one.
        let mut current = vec![(base.clone(), false)];
        let mut listed = 0;
        for name in names {
            if *name == ".." {
                return Expansion::Unknown;
            }
            let mut next = Vec::new();
            for (directory, unresolved) in &current {
                if !has_wildcards(name) {
                    match directory.join(name) {
                        Ok(path) => next.push((path, true)),
                        Err(_) => return Expansion::Unknown,
                    }
                    continue;
                }
                // The real directory, through any link to it.
                let real = if *unresolved {
                    match self.locate.locate(directory, directory.as_str()) {
                        Some(real) => real,
                        None => return Expansion::Unknown,
                    }
                } else {
                    directory.clone()
                };
                listed += 1;
                if listed > MOST_DIRECTORIES_LISTED {
                    return Expansion::TooMany;
                }
                let pattern = NamePattern::parse(name);
                match self.locate.list(&real, MOST_NAMES_LISTED) {
                    Listing::Names(found) => {
                        if !spend(found.len()) {
                            return Expansion::TooMany;
                        }
                        for (found, link) in found {
                            if pattern.could_match_name(&found, hidden_rule) {
                                match real.join(&found) {
                                    Ok(path) => next.push((path, link)),
                                    Err(_) => return Expansion::Unknown,
                                }
                            }
                        }
                    }
                    Listing::TooMany => return Expansion::TooMany,
                    Listing::Unknown => return Expansion::Unknown,
                }
            }
            if next.len() > MOST_NAMES_LISTED {
                return Expansion::TooMany;
            }
            current = next;
        }
        Expansion::Matches(current)
    }

    /// The finding for one path a pattern matched, as for the same name
    /// written out: through any link, a rule that holds it back, a
    /// protected path or a file that looks like a secret, and for a write
    /// that is certain, the scope.
    fn judge_match(
        &self,
        rules: &RuleSet,
        (path, unresolved): &(AbsPath, bool),
        shown: &str,
        how: How,
    ) -> Option<Finding> {
        let real = if *unresolved {
            let Some(real) = self.locate.locate(path, path.as_str()) else {
                return Some(Finding::ask(format!(
                    "`{shown}` names `{path}`, which is not a path that can be checked."
                )));
            };
            real
        } else {
            path.clone()
        };
        if how.reads {
            return self.judge_read_path(rules, &real, shown);
        }
        if let Some(found) = self.judge_named_path(rules, &real, shown) {
            return Some(found);
        }
        (how.scope
            && !self.in_scope(&real)
            && level::decider(self.level(), Matter::WriteBeyond) != Decider::Holder)
            .then(|| {
                Finding::ask(format!(
                    "`{shown}` writes `{real}`, which is outside the workspace."
                ))
            })
    }

    /// Judges the words of a line the analysis does not model, as they
    /// stand in the raw line (both ways [`pattern`] cuts it), for what each
    /// could name: the harness's own files are refused, and a protected
    /// path, a file that looks like a secret, or a path a rule holds back is
    /// asked about, at every level that asks about those. Nothing else of
    /// such a line is judged: the level decides it. Found by the second
    /// independent review of the fixes to step 12: `cp evil
    /// .git/hooks/pre-commit #x` ran unasked at `autonomous`.
    ///
    /// Each word is tried from every place in `from`, from the workspace,
    /// and from every directory a `cd` among the words could lead to
    /// (found by the final review of the fixes to step 12: `cd .github &&
    /// cp evil.yml workflows/ci.yml #`).
    ///
    /// [`pattern`]: crate::command::pattern
    fn judge_unmodelled(&self, rules: &RuleSet, command: &str, from: &[AbsPath]) -> Vec<Finding> {
        let shown = "The command";
        let mut places: Vec<AbsPath> = Vec::new();
        for place in from.iter().chain(std::iter::once(&self.workspace)) {
            if !places.contains(place) {
                places.push(place.clone());
            }
        }
        let mut findings = Vec::new();
        if !self.places_of_cds(command, &mut places)
            && level::decider(self.level(), Matter::WriteBeyond) != Decider::Holder
        {
            findings.push(Finding::ask(format!(
                "`{command}` changes directory where the analysis cannot follow, so what it \
                 names cannot be placed."
            )));
        }
        let how = How {
            reads: false,
            scope: false,
            listing: false,
        };
        for word in raw_words(command) {
            let found = may_name(&word).into_iter().find_map(|text| {
                let as_name = text.starts_with('~').then(|| format!("./{text}"));
                as_name
                    .as_deref()
                    .into_iter()
                    .chain(std::iter::once(text))
                    .find_map(|text| {
                        if has_wildcards(text) {
                            self.judge_pattern(rules, text, &places, shown, how)
                        } else {
                            self.judge_name(rules, text, &places, shown, false)
                        }
                    })
            });
            if let Some(found) = found {
                // A word may be text, not a path: a rule's denial is a
                // question, and the harness's own files stay refused.
                findings.push(mentioned(found));
            }
        }
        findings
    }

    /// Adds to `places` every directory a `cd` among the words of a raw
    /// line could lead to, from every place so far. `false` where one could
    /// lead anywhere: `cd -`, a pattern, more places than are followed.
    fn places_of_cds(&self, command: &str, places: &mut Vec<AbsPath>) -> bool {
        let mut placed = true;
        for cut in raw_cuts(command) {
            let mut words = cut.iter().peekable();
            while let Some(word) = words.next() {
                let program = word.rsplit(['/', '\\']).next().unwrap_or(word);
                if !matches!(
                    program.to_ascii_lowercase().as_str(),
                    "cd" | "pushd" | "chdir" | "sl" | "set-location" | "push-location"
                ) {
                    continue;
                }
                while words
                    .next_if(|word| word.len() > 1 && word.starts_with('-'))
                    .is_some()
                {}
                let targets: Vec<String> = match words.peek() {
                    None => self
                        .home
                        .iter()
                        .map(|home| home.as_str().to_owned())
                        .collect(),
                    Some(target) if *target == "-" || has_wildcards(target) => {
                        placed = false;
                        continue;
                    }
                    Some(target) if target.starts_with('~') => {
                        vec![(*target).clone(), format!("./{target}")]
                    }
                    Some(target) => vec![(*target).clone()],
                };
                for place in places.clone() {
                    for target in &targets {
                        if let Some(next) = self.locate.locate(&place, target)
                            && !places.contains(&next)
                        {
                            if places.len() >= MOST_PLACES {
                                return false;
                            }
                            places.push(next);
                        }
                    }
                }
            }
        }
        placed
    }

    /// The question for a command that names more than one decision checks.
    fn too_much(&self, shown: &str) -> Finding {
        Finding::ask(format!(
            "`{shown}` names more paths and patterns than one decision checks \
             ({MOST_WORK} names in all)."
        ))
    }

    /// The texts a word gives a path by: a `~` the shell does not take as
    /// the home directory is a name, in a folder named `~` (and in
    /// PowerShell, whose own commands read a quoted `~` as home, both).
    fn texts_of(&self, word: &crate::command::Word) -> Vec<String> {
        if word.text.starts_with('~') && !word.home {
            let mut texts = vec![format!("./{}", word.text)];
            if self.shell == ShellSyntax::PowerShell {
                texts.push(word.text.clone());
            }
            texts
        } else {
            vec![word.text.clone()]
        }
    }

    /// The separators of a path in the engine's shell.
    fn separators(&self) -> &'static [char] {
        if self.shell == ShellSyntax::PowerShell {
            &['/', '\\']
        } else {
            &['/']
        }
    }

    /// A pattern cut where its first name with a wildcard begins: the path
    /// before it, with its last separator, and the rest. `None` for a text
    /// with no wildcard.
    fn split_pattern<'t>(&self, text: &'t str) -> Option<(&'t str, &'t str)> {
        let first = text.find(['*', '?', '['])?;
        let cut = text
            .get(..first)?
            .rfind(self.separators())
            .map_or(0, |at| at + 1);
        Some(text.split_at(cut))
    }

    /// Where a pattern starts, from `place`: the directory before its first
    /// name with a wildcard. A root, a drive or a share is read as it is;
    /// `C:` alone, which depends on a current drive nobody stated, is not a
    /// place.
    fn pattern_base(&self, text: &str, place: &AbsPath) -> Option<AbsPath> {
        let (prefix, _) = self.split_pattern(text)?;
        if prefix.is_empty() {
            return Some(place.clone());
        }
        let trimmed = prefix.trim_end_matches(self.separators());
        if trimmed.is_empty() || trimmed.ends_with(':') {
            return AbsPath::parse(prefix, place.style()).ok();
        }
        self.locate.locate(place, trimmed)
    }

    /// The finding for a redirection to a pattern that starts outside the
    /// lease's scope, from any of `places`: a write there, as for a file
    /// named outright (found by the second independent review of the fixes
    /// to step 12: `echo key >> ~/.ssh/authorized_key?`).
    fn pattern_outside(&self, text: &str, places: &[AbsPath], shown: &str) -> Option<Finding> {
        if level::decider(self.level(), Matter::WriteBeyond) == Decider::Holder {
            return None;
        }
        places
            .iter()
            .filter_map(|place| self.pattern_base(text, place))
            .find(|base| !self.in_scope(base))
            .map(|base| {
                Finding::ask(format!(
                    "`{shown}` writes `{text}`, a pattern under `{base}`, which is outside the \
                     workspace."
                ))
            })
    }

    /// Judges one simple command, run in `here`: how it is allowed, and
    /// what its redirections and the paths it names come to.
    fn judge_part(
        &self,
        rules: &RuleSet,
        part: &Part,
        here: &AbsPath,
        listing: bool,
    ) -> (Standing, Vec<Finding>) {
        let shown = words_of(part).join(" ");
        let mut findings = Vec::new();
        let mut writes = false;
        let mut to_pattern = false;
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
            if !target.is_fixed() && has_wildcards(&target.text) {
                // A pattern: the shell may put any file's name it matches in
                // its place. The part is on no safe list, and what the
                // pattern could match is checked as for an argument.
                to_pattern = true;
                writes = true;
                let reads = effect == Effect::Read;
                let at = std::slice::from_ref(here);
                let how = How {
                    reads,
                    scope: !reads,
                    listing,
                };
                if let Some(found) = self.judge_pattern(rules, &target.text, at, &shown, how) {
                    findings.push(found);
                }
                if !reads && let Some(found) = self.pattern_outside(&target.text, at, &shown) {
                    findings.push(found);
                }
                continue;
            }
            for text in self.texts_of(target) {
                match self.locate.locate(here, &text) {
                    Some(path) => {
                        findings.push(self.judge_path(rules, &effect, &path, Some(&shown)));
                    }
                    None => findings.push(Finding::ask(format!(
                        "`{shown}` redirects to or from `{text}`, which is not a path that can be \
                         checked."
                    ))),
                }
            }
        }
        if to_pattern {
            findings.push(self.other_command(format!(
                "`{shown}` redirects to or from a pattern, which the shell may replace with any \
                 file's name it matches, so no rule and no safe list applies to it."
            )));
        }

        let named = table::read_only(part, self.shell, self.repository_trusted);
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
                // A quoted `~` is a name: both are read, as the analysis
                // cannot tell here which it was.
                let quoted_home = reading
                    .named
                    .iter()
                    .filter(|name| name.text.starts_with('~'))
                    .map(|name| table::Named {
                        text: format!("./{}", name.text),
                        must_resolve: false,
                    });
                let named: Vec<table::Named> = quoted_home.collect();
                for name in reading.named.into_iter().chain(named) {
                    match self.locate.locate(here, &name.text) {
                        Some(path) => {
                            findings.push(mentioned(self.judge_path(
                                rules,
                                &Effect::Read,
                                &path,
                                Some(&shown),
                            )));
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
                let how = How {
                    reads: !writes && self.only_reads_but_for_patterns(part),
                    scope: false,
                    listing,
                };
                findings.extend(self.judge_named(
                    rules,
                    part,
                    std::slice::from_ref(here),
                    None,
                    &shown,
                    how,
                ));
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
        let (mut removed, stored) = {
            let mut rules = self.rules.write().unwrap_or_else(PoisonError::into_inner);
            let removed = rules.session.remove(ids);
            self.recompile(&mut rules);
            let stored = rules
                .approved
                .iter()
                .any(|approval| ids.contains(&approval.id));
            (removed, stored)
        };
        if stored {
            // The file is changed without the rules held, so that deciding
            // a call does not wait on another session's lock on the file.
            if let Some(store) = &self.store {
                store.remove(ids)?;
            }
            let mut rules = self.rules.write().unwrap_or_else(PoisonError::into_inner);
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

/// Whether a path is build or package configuration that its program reads
/// from the directory it starts in and upward, wherever that is:
/// `.cargo/config.toml` and `.cargo/config` (cargo), `.npmrc` (npm, for a
/// package below it), `.yarnrc` and `.yarnrc.yml` (yarn), and `.envrc`
/// (direnv). A build started below one reads it, so one in a subdirectory
/// names a program to run, or a registry to send a token to, as surely as
/// one at the workspace root (found by the review of step 12). Compared
/// without regard to case: protecting too much is the safe side.
///
/// The other protected names are read from one place: a shell's start-up
/// files from the home directory, CI configuration and the repository's
/// hooks from its root, `.cortex/` from the workspace's.
fn is_build_configuration(path: &AbsPath) -> bool {
    let mut names = path.names().rev();
    let Some(last) = names.next() else {
        return false;
    };
    let in_cargo = names
        .next()
        .is_some_and(|parent| parent.eq_ignore_ascii_case(".cargo"));
    [".npmrc", ".yarnrc", ".yarnrc.yml", ".envrc"]
        .iter()
        .any(|name| last.eq_ignore_ascii_case(name))
        || (in_cargo
            && (last.eq_ignore_ascii_case("config.toml") || last.eq_ignore_ascii_case("config")))
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
fn combine(findings: Vec<Finding>, read_only: bool, storable: bool) -> Outcome {
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
    Outcome::Ask(Question {
        reasons,
        lifts,
        storable,
    })
}

/// The texts in an argument of a command off the table that may be a path
/// the program opens, for checking each against what is protected:
///
/// - the argument itself, unless it is an option;
/// - the value after each `=`: `--target-directory=.git/hooks`;
/// - for an option of one dash, every text from its third character on.
///   A short option takes its value attached, after one letter or after a
///   cluster of them (`cp -t.git/hooks`, `sort -o.git/config`, `tar
///   -C.git/hooks`, `cp -vt.git/hooks`), and the analysis does not know the
///   letters of every program, so it tries each place the value could
///   begin (found by the review of step 12). Too many tries can only ask
///   once too often;
/// - the value after the first `:` of an option written with a `/`, as
///   Windows programs take theirs: `/out:file`.
fn may_name(text: &str) -> Vec<&str> {
    let mut texts = Vec::new();
    if !text.starts_with('-') {
        texts.push(text);
    }
    // The value after each `=`: `dd of=.github/workflows/a=b.yml`.
    for (at, _) in text.match_indices('=') {
        texts.push(&text[at + 1..]);
    }
    if text.starts_with('-') && !text.starts_with("--") {
        texts.extend(text.char_indices().skip(2).map(|(at, _)| &text[at..]));
    }
    if text.starts_with('/')
        && let Some((_, value)) = text.split_once(':')
    {
        texts.push(value);
    }
    texts.retain(|text| !text.is_empty());
    texts
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

/// Programs an approval covers only as the exact line, word for word:
/// "always allow `sudo *`" is not what a person meant by approving `sudo
/// apt list`, nor "always allow `curl *`" by approving one download.
///
/// - Programs whose arguments are themselves a command, a program's text,
///   or a script in a language of their own: wrappers, shells,
///   interpreters, editors with a command language, `find`, `sed`, `tar`.
/// - Programs that reach the network, or fetch a package and run it: a
///   download can be written anywhere (`curl -o ~/.bashrc`), and an upload
///   can send anything.
/// - Windows programs and PowerShell commands that start or schedule
///   others, or download.
const EXACT_LINE: [&str; 152] = [
    // Wrappers that run what follows them.
    "sudo",
    "doas",
    "su",
    "runuser",
    "pkexec",
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
    "nsenter",
    "flock",
    "taskset",
    "numactl",
    "chrt",
    "prlimit",
    "faketime",
    "sg",
    "strace",
    "ltrace",
    "gdb",
    "lldb",
    "script",
    "busybox",
    "parallel",
    "watchexec",
    "entr",
    "systemd-run",
    "firejail",
    "bwrap",
    "xvfb-run",
    "dbus-run-session",
    "tmux",
    "screen",
    "at",
    "batch",
    "crontab",
    "docker",
    "podman",
    "kubectl",
    "ssh",
    "git-shell",
    // Shells.
    "sh",
    "bash",
    "dash",
    "zsh",
    "ksh",
    "fish",
    "pwsh",
    "powershell",
    "cmd",
    "wsl",
    // Interpreters, and programs with a language of their own.
    "python",
    "python3",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "lua",
    "java",
    "tclsh",
    "rscript",
    "julia",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "m4",
    "sqlite3",
    "psql",
    "mysql",
    "find",
    "sed",
    "tar",
    "rsync",
    // Editors whose arguments can be commands (`vim -c '!…'`).
    "vi",
    "vim",
    "nvim",
    "ex",
    "ed",
    "emacs",
    // Fetching a package and running it.
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "pipx",
    // The network.
    "curl",
    "wget",
    "aria2c",
    "scp",
    "sftp",
    "ftp",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "telnet",
    // Windows, and PowerShell's own.
    "runas",
    "cscript",
    "wscript",
    "mshta",
    "rundll32",
    "msiexec",
    "certutil",
    "bitsadmin",
    "schtasks",
    "start-process",
    "invoke-command",
    "invoke-webrequest",
    "invoke-restmethod",
    // Programs with an option that runs another (found by the independent
    // review of the fixes to step 12): `rg --pre`, `fd -x`, `man -P`, a
    // pager's `!`, `zip -TT`; more interpreters; and cloud tools, which
    // reach the network.
    "rg",
    "fd",
    "fdfind",
    "man",
    "less",
    "more",
    "ts-node",
    "tsx",
    "pypy",
    "pypy3",
    "zip",
    "aws",
    "rclone",
    "gsutil",
    "gcloud",
    "az",
    "s3cmd",
    // PowerShell's names for starting and fetching, and more programs that
    // run what follows them (found by the second independent review of the
    // fixes to step 12).
    "iwr",
    "irm",
    "saps",
    "start",
    "icm",
    "ii",
    "invoke-item",
    "start-job",
    "py",
    "nodejs",
    "perf",
    "valgrind",
    "hyperfine",
    "nix-shell",
    "nix",
    "unbuffer",
];

/// Programs that do their work through subcommands, each with the
/// subcommands an approval may cover with any further words (`cargo test
/// *`): those whose further words choose what is built, shown or changed in
/// the project, and cannot name a program to run, a package to fetch and
/// run, or a place to send the project to.
///
/// Any other subcommand of these programs, and the program with no
/// subcommand, is approved as the exact line. Some that are not listed, and
/// why (found by the review of step 12): `npm exec`, `pnpm dlx` and `yarn
/// dlx` fetch a package and run it; `git rebase --exec`, `git bisect run`,
/// `git submodule foreach`, `cargo watch -x` and `rustup run` run a command
/// given in their arguments; `git push`, `git fetch`, `git pull` and `git
/// clone` reach the network and take a program to run there
/// (`--receive-pack`, `--upload-pack`); `git grep -O` starts a pager; `git
/// init --template` copies in hooks; `go run` fetches a module and runs it.
const SUBCOMMANDS: [(&str, &[&str]); 25] = [
    (
        "git",
        &[
            "add",
            "blame",
            "branch",
            "checkout",
            "cherry-pick",
            "clean",
            "commit",
            "describe",
            "diff",
            "log",
            "ls-files",
            "merge",
            "mv",
            "reflog",
            "reset",
            "restore",
            "revert",
            "rm",
            "shortlog",
            "show",
            "stash",
            "status",
            "switch",
            "tag",
        ],
    ),
    (
        "cargo",
        &[
            "bench", "build", "check", "clean", "clippy", "doc", "fmt", "metadata", "run", "test",
            "tree",
        ],
    ),
    ("npm", &["ls", "run", "run-script", "start", "test"]),
    ("pnpm", &["list", "ls", "run", "start", "test"]),
    ("yarn", &["run", "start", "test"]),
    (
        "go",
        &["build", "doc", "fmt", "list", "test", "version", "vet"],
    ),
    ("dotnet", &["build", "format", "run", "test"]),
    ("pip", &["freeze", "list", "show"]),
    ("pip3", &["freeze", "list", "show"]),
    ("poetry", &["build", "check", "show"]),
    ("uv", &["build", "lock", "sync", "tree"]),
    ("gh", &[]),
    ("apt", &[]),
    ("apt-get", &[]),
    ("brew", &[]),
    ("rustup", &["show"]),
    ("hg", &["diff", "log", "status"]),
    ("svn", &["diff", "info", "log", "status"]),
    ("conda", &[]),
    ("systemctl", &["status"]),
    ("dnf", &[]),
    ("just", &[]),
    // `bundle exec`, `pipenv run`, `direnv exec`: a command given.
    ("bundle", &[]),
    ("pipenv", &[]),
    ("direnv", &[]),
];

/// The allow rule an approval of this command adds: the program and its
/// subcommand, with any further words, or the exact line ([`EXACT_LINE`],
/// [`SUBCOMMANDS`]). `None` when no pattern says that safely.
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
    // `npm.cmd` is `npm`, as Windows finds it.
    let name = [".exe", ".cmd", ".bat", ".com", ".ps1"]
        .iter()
        .find_map(|ending| name.strip_suffix(ending))
        .unwrap_or(&name);
    // `python3.12` is `python`, and `ruby3.2` is `ruby`: a version after
    // the name changes nothing about what the program does with its
    // arguments.
    let unversioned =
        name.trim_end_matches(|character: char| character.is_ascii_digit() || character == '.');
    let listed = |list: &[&str]| list.contains(&name) || list.contains(&unversioned);
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
    let exact_line = listed(&EXACT_LINE)
        || SUBCOMMANDS
            .iter()
            .find(|(program, _)| *program == name || *program == unversioned)
            .is_some_and(|(_, covered)| {
                !subcommand.is_some_and(|subcommand| covered.contains(&subcommand.text.as_str()))
            });
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
            ("git commit -m fix", Some("git commit *")),
            ("git status", Some("git status *")),
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
            ("git", Some("git")),
            // A subcommand that is not listed as safe with any further
            // words: the exact line. Found by the review of step 12.
            ("npm exec cowsay", Some("npm exec cowsay")),
            ("pnpm dlx cowsay", Some("pnpm dlx cowsay")),
            ("yarn dlx cowsay", Some("yarn dlx cowsay")),
            ("git rebase main", Some("git rebase main")),
            ("git bisect start", Some("git bisect start")),
            ("git submodule update", Some("git submodule update")),
            ("git push origin main", Some("git push origin main")),
            ("git fetch origin", Some("git fetch origin")),
            ("git init", Some("git init")),
            ("rustup run nightly cargo", Some("rustup run nightly cargo")),
            ("cargo watch -x build", Some("cargo watch -x build")),
            ("go run main.go", Some("go run main.go")),
            ("yarn build", Some("yarn build")),
            // The network, an editor, an interpreter: the exact line.
            ("curl http://x.example", Some("curl http://x.example")),
            ("wget http://x.example", Some("wget http://x.example")),
            ("scp a host:b", Some("scp a host:b")),
            ("nc host 80", Some("nc host 80")),
            ("vim file", Some("vim file")),
            ("java Main", Some("java Main")),
            // A version after the name is the same program, and programs
            // with an option that runs another. Found by the independent
            // review of the fixes to step 12.
            ("python3.12 x.py", Some("python3.12 x.py")),
            ("pip3.12 install x", Some("pip3.12 install x")),
            ("rg foo", Some("rg foo")),
            ("fd -x ls", Some("fd -x ls")),
            ("man ls", Some("man ls")),
            ("aws s3 ls", Some("aws s3 ls")),
            ("npm.cmd exec x", Some("npm.cmd exec x")),
            ("bundle exec rake", Some("bundle exec rake")),
            ("valgrind ./app", Some("valgrind ./app")),
            (
                "CURL.EXE http://x.example",
                Some("CURL.EXE http://x.example"),
            ),
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
