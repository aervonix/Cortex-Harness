//! Integration tests (tier 2): the permission engine as the core will use
//! it, the stored files on a real file system, and command analysis checked
//! against the real shells.
//!
//! Most tests decide calls for a workspace at `/work/app` whose paths are
//! read as text, so they are the same on every OS. The modules that touch
//! the file system work in a directory of their own under Cargo's temporary
//! directory. No test waits: time comes from the fake clock.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "an integration test prepares real files, fails by panicking, and says when it skips"
)]

mod approvals;
mod bypasses;
mod commands;
mod differential;
mod files;
mod levels;
mod measure;
mod real_paths;
mod trust;

use std::sync::Arc;

use cortex_base::{AbsPath, FakeClock, PathStyle, SharedClock};
use cortex_policy::command::ShellSyntax;
use cortex_policy::{
    Access, Engine, Lexical, Outcome, Parts, Protected, Question, Request, RuleSource,
    SandboxState, SourcedRule, default_sandbox,
};
use cortex_protocol::ids::{LeaseId, ProjectKey};
use cortex_protocol::lease::{Budget, Effect, HardLimits, Level, Rule, RuleAction};
pub(crate) use cortex_testkit::Scratch;

/// The test this binary is started into by a test that needs a program
/// that prints its arguments (`cortex_testkit::children`).
#[cfg(windows)]
pub(crate) const ENTRY: &str = "child_entry";

/// Run by the test runner, it passes at once. Started by a test with the
/// arguments `cortex_testkit::children::echo_arguments` gives, it prints
/// the rest of its arguments and exits.
#[cfg(windows)]
#[test]
fn child_entry() {
    cortex_testkit::children::run_if_child();
}

/// The workspace of the tests that read paths as text.
pub(crate) const WORKSPACE: &str = "/work/app";
/// Their home directory.
pub(crate) const HOME: &str = "/users/dev";

pub(crate) fn path(text: &str) -> AbsPath {
    AbsPath::parse(text, PathStyle::Posix).unwrap()
}

pub(crate) fn clock() -> SharedClock {
    Arc::new(FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap())
}

pub(crate) fn rule(effect: Effect, pattern: &str, action: RuleAction) -> Rule {
    Rule {
        effect,
        pattern: pattern.to_owned(),
        action,
    }
}

pub(crate) fn user(effect: Effect, pattern: &str, action: RuleAction) -> SourcedRule {
    SourcedRule::new(rule(effect, pattern, action), RuleSource::User)
}

pub(crate) fn project(effect: Effect, pattern: &str, action: RuleAction) -> SourcedRule {
    SourcedRule::new(rule(effect, pattern, action), RuleSource::Project)
}

/// The harness's own directories, as the tests have them.
pub(crate) fn protected() -> Protected {
    Protected::none()
        .closed(path("/users/dev/.config/cortex-harness"))
        .closed(path("/users/dev/.local/state/cortex-harness"))
        .unwritable(path("/users/dev/.local/share/cortex-harness"))
        .other_sessions(
            path("/users/dev/.local/share/cortex-harness/sessions"),
            ProjectKey::new("aaaa1111"),
        )
}

/// What an engine of these tests is made from: `supervised`, trusted, a
/// POSIX shell, an enforcing sandbox, no rules. Each test changes what it
/// is about.
pub(crate) fn parts() -> Parts {
    let workspace = path(WORKSPACE);
    Parts {
        lease_id: LeaseId::new("l_1"),
        level: Level::Supervised,
        rules: Vec::new(),
        approved: Vec::new(),
        sandbox_policy: default_sandbox(&workspace, &["/tmp/cortex-s1".to_owned()], &[]),
        sandbox: SandboxState::Enforcing,
        models: Vec::new(),
        budget: Budget::default(),
        limits: HardLimits::default(),
        workspace,
        home: Some(path(HOME)),
        trusted: true,
        protected: protected(),
        shell: ShellSyntax::Posix,
        locate: Arc::new(Lexical {
            home: Some(path(HOME)),
        }),
        store: None,
        clock: clock(),
    }
}

pub(crate) fn engine_with(change: impl FnOnce(&mut Parts)) -> Engine {
    let mut parts = parts();
    change(&mut parts);
    Engine::start(parts).unwrap()
}

pub(crate) fn engine() -> Engine {
    engine_with(|_| {})
}

pub(crate) fn at(level: Level) -> Engine {
    engine_with(|parts| parts.level = level)
}

pub(crate) fn with_rules(rules: Vec<SourcedRule>) -> Engine {
    engine_with(|parts| parts.rules = rules)
}

/// What the `shell` tool declares for a command: the command, and a write
/// of the workspace root, and of the directory it runs in when that is
/// outside the workspace.
pub(crate) fn shell_in(engine: &Engine, command: &str, cwd: &str) -> Outcome {
    let cwd = path(cwd);
    let mut accesses = vec![
        Access::Exec {
            command: command.to_owned(),
            cwd: cwd.clone(),
        },
        Access::Write {
            path: path(WORKSPACE),
        },
    ];
    if !cwd.starts_with(&path(WORKSPACE)) {
        accesses.push(Access::Write { path: cwd });
    }
    engine.decide(&Request {
        tool: "shell",
        // As the tool's spec has it.
        default: RuleAction::Ask,
        accesses: &accesses,
    })
}

pub(crate) fn shell(engine: &Engine, command: &str) -> Outcome {
    shell_in(engine, command, WORKSPACE)
}

pub(crate) fn read(engine: &Engine, file: &str) -> Outcome {
    engine.decide(&Request {
        tool: "read",
        default: RuleAction::Allow,
        accesses: &[Access::Read { path: path(file) }],
    })
}

pub(crate) fn write(engine: &Engine, file: &str) -> Outcome {
    engine.decide(&Request {
        tool: "write",
        default: RuleAction::Allow,
        accesses: &[Access::Write { path: path(file) }],
    })
}

/// An outcome in one word, for tables of expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Said {
    /// Allowed, and marked as a command that only reads.
    ReadOnly,
    /// Allowed.
    Allow,
    /// Asked about.
    Ask,
    /// Refused.
    Deny,
}

pub(crate) fn said(outcome: &Outcome) -> Said {
    match outcome {
        Outcome::Allow { read_only: true } => Said::ReadOnly,
        Outcome::Allow { read_only: false } => Said::Allow,
        Outcome::Ask(_) => Said::Ask,
        Outcome::Deny { .. } => Said::Deny,
    }
}

pub(crate) fn question(outcome: Outcome) -> Question {
    match outcome {
        Outcome::Ask(question) => question,
        other => panic!("not asked about: {other:?}"),
    }
}

pub(crate) fn reason(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ask(question) => question.reasons.join(" "),
        Outcome::Deny { reason } => reason.clone(),
        Outcome::Allow { .. } => String::new(),
    }
}

/// Whether this is the CI job, where every case must really run.
pub(crate) fn in_ci() -> bool {
    std::env::var_os("GITHUB_ACTIONS").is_some()
}
