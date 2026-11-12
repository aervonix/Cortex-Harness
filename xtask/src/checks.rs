//! The check and test commands. The blocking CI jobs run `check` and
//! `ci test`, so the commands here are the definition of what CI runs
//! (`docs/testing-and-ci.md`).

use std::process::{Command, Stdio};

use crate::Result;
use crate::rules;
use crate::steps::{self, Step};
use crate::tiers;
use crate::workspace;

/// The tier 0 steps, in the order they run: cheapest first.
const CHECK_STEPS: [&str; 4] = ["fmt", "rules", "clippy", "deny"];

fn check_step(name: &str) -> Option<Step> {
    Some(match name {
        "fmt" => Step::cargo("fmt", &["fmt", "--all", "--check"]),
        "rules" => Step::function("rules", rules),
        "clippy" => Step::cargo(
            "clippy",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        ),
        // Advisories are not here: a new advisory against an unchanged
        // lockfile must not block unrelated pull requests. They run nightly.
        "deny" => Step::cargo("deny", &["deny", "check", "licenses", "bans", "sources"]),
        _ => return None,
    })
}

/// `cargo xtask check [STEP...]`
pub(crate) fn check(arguments: &[String]) -> Result<()> {
    let names: Vec<&str> = if arguments.is_empty() {
        CHECK_STEPS.to_vec()
    } else {
        arguments.iter().map(String::as_str).collect()
    };
    let mut selected = Vec::new();
    for name in names {
        selected.push(check_step(name).ok_or_else(|| {
            format!(
                "unknown check `{name}`. The checks are: {}",
                CHECK_STEPS.join(", ")
            )
        })?);
    }
    steps::run("Static checks", selected)
}

/// `cargo xtask rules`
pub(crate) fn rules() -> Result<()> {
    let violations = rules::check_repository(&workspace::root())?;
    if violations.is_empty() {
        println!("The repository follows its rules.");
        return Ok(());
    }
    for violation in &violations {
        println!("{violation}\n");
    }
    Err(format!("{} repo-rule violation(s)", violations.len()))
}

/// `cargo xtask fmt`
pub(crate) fn fmt() -> Result<()> {
    steps::cargo(&["fmt", "--all"])
}

/// `cargo xtask test [ARGS...]`
pub(crate) fn test(arguments: &[String]) -> Result<()> {
    let mut command = vec![
        "nextest".to_owned(),
        "run".to_owned(),
        "--workspace".to_owned(),
    ];
    command.extend(arguments.iter().cloned());
    require_nextest()?;
    steps::cargo(&command)
}

/// Fails with a pointer to the installer when cargo-nextest is missing,
/// instead of cargo's "no such command".
fn require_nextest() -> Result<()> {
    let found = Command::new(workspace::cargo())
        .args(["nextest", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if found {
        return Ok(());
    }
    Err(
        "cargo-nextest is not installed. See https://nexte.st. The version CI uses is in \
         docs/dependencies.md."
            .to_owned(),
    )
}

/// The blocking test job: tiers 1 to 3, in nextest's `ci` profile, built with
/// cargo's `ci` profile (third-party crates unoptimised, so that a cold cache
/// still fits the time budget). Building and running are separate steps so
/// that their times are reported separately.
fn ci_test_steps() -> Vec<Step> {
    const RUN: [&str; 8] = [
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--cargo-profile",
        "ci",
        "--profile",
        "ci",
    ];
    let mut build = RUN.to_vec();
    build.push("--no-run");
    vec![
        Step::cargo("build tests", &build),
        Step::cargo("run tests", &RUN),
        // Each test has its own limit in .config/nextest.toml. This checks
        // what the tests of each tier took together.
        Step::function("tier budgets", tiers::check_ci_report),
    ]
}

/// `cargo xtask ci [lint|test]`
pub(crate) fn ci(arguments: &[String]) -> Result<()> {
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["lint"] => check(&[]),
        ["test"] => {
            require_nextest()?;
            steps::run("Tests", ci_test_steps())
        }
        [] => {
            require_nextest()?;
            let mut all: Vec<Step> = CHECK_STEPS
                .iter()
                .filter_map(|name| check_step(name))
                .collect();
            all.extend(ci_test_steps());
            steps::run("Everything that blocks a pull request", all)
        }
        _ => Err("usage: cargo xtask ci [lint|test]".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{CHECK_STEPS, check, check_step};

    #[test]
    fn every_listed_check_exists() {
        for name in CHECK_STEPS {
            assert!(check_step(name).is_some(), "{name}");
        }
    }

    #[test]
    fn an_unknown_check_is_refused_before_anything_runs() {
        let error = check(&["clipy".to_owned()]).unwrap_err();
        assert!(error.contains("unknown check `clipy`"));
        assert!(error.contains("fmt, rules, clippy, deny"));
    }
}
