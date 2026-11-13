//! `cargo xtask decide`: the engine's decision on one command line, with its
//! reasons, for a review or a fix that wants to check a case without writing
//! a test (`docs/testing-and-ci.md`, "Asking the permission engine").
//!
//! The probe is an ignored test of this binary, so that it builds nothing
//! the blocking checks do not already build. `xtask` runs it with the case
//! in the environment (`CORTEX_DECIDE_*`) and it prints the decision. The
//! engine is the one the tests of the command table use: a workspace at
//! `/work/app`, a home at `/users/dev`, a trusted project, an enforcing
//! sandbox, unless the case says otherwise.

use std::fmt::Write as _;

use cortex_policy::command::{ShellSyntax, analyse};
use cortex_policy::{Engine, Outcome, RuleSource, SourcedRule};
use cortex_protocol::lease::{Effect, Level, RuleAction};

use crate::{WORKSPACE, path, rule, shell_in};

/// One case to decide.
#[derive(Debug, Clone)]
pub(crate) struct Case {
    pub(crate) command: String,
    pub(crate) level: Level,
    pub(crate) rules: Vec<SourcedRule>,
    pub(crate) workspace: String,
    pub(crate) cwd: Option<String>,
    pub(crate) shell: ShellSyntax,
    pub(crate) trusted: bool,
}

impl Case {
    pub(crate) fn of(command: &str) -> Self {
        Case {
            command: command.to_owned(),
            level: Level::Supervised,
            rules: Vec::new(),
            workspace: WORKSPACE.to_owned(),
            cwd: None,
            shell: ShellSyntax::Posix,
            trusted: true,
        }
    }

    /// The case `cargo xtask decide` put in the environment.
    fn from_environment() -> Result<Self, String> {
        let variable = |name: &str| std::env::var(format!("CORTEX_DECIDE_{name}")).ok();
        let command = variable("COMMAND").ok_or("CORTEX_DECIDE_COMMAND is not set")?;
        let mut case = Case::of(&command);
        if let Some(level) = variable("LEVEL") {
            case.level = serde_json::from_value(serde_json::Value::String(level.clone()))
                .map_err(|_| format!("`{level}` is not a level"))?;
        }
        if let Some(rules) = variable("RULES") {
            case.rules = rules
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(parse_rule)
                .collect::<Result<_, _>>()?;
        }
        if let Some(workspace) = variable("WORKSPACE") {
            case.workspace = workspace;
        }
        case.cwd = variable("CWD");
        if let Some(shell) = variable("SHELL") {
            case.shell = match shell.as_str() {
                "posix" => ShellSyntax::Posix,
                "powershell" => ShellSyntax::PowerShell,
                "cmd" => ShellSyntax::Cmd,
                other => return Err(format!("`{other}`: posix, powershell or cmd")),
            };
        }
        case.trusted = variable("UNTRUSTED").is_none();
        Ok(case)
    }
}

/// `user allow exec cargo build *`: who wrote the rule, its action, its
/// effect and its pattern.
pub(crate) fn parse_rule(line: &str) -> Result<SourcedRule, String> {
    let mut words = line.trim().splitn(4, ' ');
    let (Some(source), Some(action), Some(effect), Some(pattern)) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Err(format!(
            "`{line}`: a rule is `user|project allow|ask|deny read|write|exec|network|ext PATTERN`"
        ));
    };
    let source = match source {
        "user" => RuleSource::User,
        "project" => RuleSource::Project,
        other => return Err(format!("`{other}`: a rule is the user's or the project's")),
    };
    let read = |text: &str| serde_json::Value::String(text.to_owned());
    let action: RuleAction =
        serde_json::from_value(read(action)).map_err(|_| format!("`{action}` is not an action"))?;
    let effect: Effect =
        serde_json::from_value(read(effect)).map_err(|_| format!("`{effect}` is not an effect"))?;
    Ok(SourcedRule::new(
        rule(effect, pattern.trim(), action),
        source,
    ))
}

/// The engine's decision on `case`, with the analysis it stands on.
pub(crate) fn decide(case: &Case) -> Result<String, String> {
    let workspace = path(&case.workspace);
    let mut parts = crate::parts();
    parts.level = case.level.clone();
    parts.rules = case.rules.clone();
    parts.workspace = workspace.clone();
    parts.sandbox_policy =
        cortex_policy::default_sandbox(&workspace, &["/tmp/cortex-s1".to_owned()], &[]);
    parts.shell = case.shell;
    parts.trusted = case.trusted;
    parts.repository_trusted = case.trusted;
    let engine = Engine::start(parts)
        .map_err(|error| format!("the engine would not start with these settings: {error}"))?;
    let cwd = case.cwd.clone().unwrap_or_else(|| case.workspace.clone());
    let outcome = shell_in(&engine, &case.command, &cwd);

    let mut text = format!(
        "command   {}\nlevel     {}\nworkspace {}  (cwd {cwd}, {}, {:?} syntax)\n",
        case.command,
        serde_json::to_value(&case.level)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default(),
        case.workspace,
        if case.trusted {
            "trusted"
        } else {
            "not trusted"
        },
        case.shell,
    );
    for rule in &case.rules {
        let _ = writeln!(text, "rule      {rule:?}");
    }
    let _ = writeln!(text, "analysis  {:?}", analyse(&case.command, case.shell));
    match &outcome {
        Outcome::Allow { read_only } => {
            let _ = writeln!(
                text,
                "decision  ALLOW{}",
                if *read_only { ", read-only" } else { "" }
            );
        }
        Outcome::Ask(question) => {
            let _ = writeln!(text, "decision  ASK");
            for reason in &question.reasons {
                let _ = writeln!(text, "  because {reason}");
            }
            for lift in &question.lifts {
                let _ = writeln!(text, "  an approval would add: {lift:?}");
            }
        }
        Outcome::Deny { reason } => {
            let _ = writeln!(text, "decision  DENY\n  because {reason}");
        }
    }
    Ok(text)
}

/// The probe `cargo xtask decide` runs.
#[test]
#[ignore = "run by `cargo xtask decide`, with the case in the environment"]
fn probe() {
    // Run with the other ignored tests, it has no case and says nothing.
    if std::env::var_os("CORTEX_DECIDE_COMMAND").is_none() {
        return;
    }
    match Case::from_environment().and_then(|case| decide(&case)) {
        Ok(text) => print!("{text}"),
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn the_probe_says_what_the_command_table_says() {
    // Rows of `commands::the_command_table_at_the_default_level`.
    for (command, decision) in [
        ("ls -la src", "decision  ALLOW, read-only"),
        ("echo hello > notes.txt", "decision  ALLOW\n"),
        ("cargo build", "decision  ASK"),
        ("cd src && ls", "decision  ASK"),
    ] {
        let text = decide(&Case::of(command)).unwrap();
        assert!(text.contains(decision), "{command}: {text}");
    }
    // A rule, as the probe reads it, decides as the table's rules do.
    let mut allowed = Case::of("cargo build --release");
    allowed.rules = vec![parse_rule("user allow exec cargo build *").unwrap()];
    assert!(decide(&allowed).unwrap().contains("decision  ALLOW\n"));
    let mut denied = Case::of("git push origin main");
    denied.rules = vec![parse_rule("project deny exec git push *").unwrap()];
    let text = decide(&denied).unwrap();
    assert!(text.contains("decision  DENY"), "{text}");
    // Read-only refuses an edit by redirection.
    let mut read_only = Case::of("echo hi > notes.txt");
    read_only.level = Level::ReadOnly;
    assert!(!decide(&read_only).unwrap().contains("ALLOW"));
    // A rule that is not one is refused, with the form a rule takes.
    let error = parse_rule("user allow cargo").unwrap_err();
    assert!(error.contains("user|project"), "{error}");
    assert!(parse_rule("someone allow exec x").is_err());
    assert!(parse_rule("user permit exec x").is_err());
}
