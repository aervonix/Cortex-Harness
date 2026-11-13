//! Running a list of named steps: every step runs even if an earlier one
//! failed, each is timed, and the timings are printed at the end. A CI round
//! trip is slow, so one run should report everything that is wrong.

use std::env;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::Result;
use crate::workspace;

/// What a step does.
pub(crate) enum Action {
    /// `cargo <arguments>`, run in the repository root.
    Cargo(Vec<String>),
    /// A check implemented in this crate.
    Function(fn() -> Result<()>),
    /// A check implemented in this crate that was given arguments.
    Closure(Box<dyn Fn() -> Result<()>>),
}

/// One named, timed step.
pub(crate) struct Step {
    pub(crate) name: &'static str,
    pub(crate) action: Action,
}

impl Step {
    pub(crate) fn cargo(name: &'static str, arguments: &[&str]) -> Self {
        let arguments = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();
        Self {
            name,
            action: Action::Cargo(arguments),
        }
    }

    /// `cargo <arguments>`, where the arguments were put together at run
    /// time.
    pub(crate) fn cargo_owned(name: &'static str, arguments: Vec<String>) -> Self {
        Self {
            name,
            action: Action::Cargo(arguments),
        }
    }

    pub(crate) fn function(name: &'static str, function: fn() -> Result<()>) -> Self {
        Self {
            name,
            action: Action::Function(function),
        }
    }

    pub(crate) fn closure(name: &'static str, closure: impl Fn() -> Result<()> + 'static) -> Self {
        Self {
            name,
            action: Action::Closure(Box::new(closure)),
        }
    }
}

/// Runs `cargo <arguments>` in the repository root, with its output going
/// straight to the terminal.
pub(crate) fn cargo<S: AsRef<OsStr>>(arguments: &[S]) -> Result<()> {
    cargo_with(arguments, &[])
}

/// [`cargo`], with environment variables set for the command.
pub(crate) fn cargo_with<S: AsRef<OsStr>>(
    arguments: &[S],
    variables: &[(&str, &str)],
) -> Result<()> {
    let shown: Vec<String> = arguments
        .iter()
        .map(|argument| argument.as_ref().to_string_lossy().into_owned())
        .collect();
    let shown = shown.join(" ");
    println!("$ cargo {shown}");
    let status = Command::new(workspace::cargo())
        .args(arguments)
        .envs(variables.iter().copied())
        .current_dir(workspace::root())
        .status()
        .map_err(|error| format!("could not run cargo {shown}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {shown}` failed"))
    }
}

/// Runs the steps in order and reports. Fails if any step failed.
pub(crate) fn run(title: &str, steps: Vec<Step>) -> Result<()> {
    let in_actions = env::var_os("GITHUB_ACTIONS").is_some();
    let mut results: Vec<(&'static str, Duration, Result<()>)> = Vec::new();

    for step in steps {
        if in_actions {
            println!("::group::{}", step.name);
        } else {
            println!("\n== {} ==", step.name);
        }
        let started = Instant::now();
        let outcome = match &step.action {
            Action::Cargo(arguments) => cargo(arguments),
            Action::Function(function) => function(),
            Action::Closure(closure) => closure(),
        };
        let elapsed = started.elapsed();
        if in_actions {
            println!("::endgroup::");
        }
        if let Err(message) = &outcome {
            // Outside the group, so that it is visible without expanding it.
            println!("{}: FAILED: {message}", step.name);
        }
        results.push((step.name, elapsed, outcome));
    }

    let total: Duration = results.iter().map(|(_, elapsed, _)| *elapsed).sum();
    let width = results
        .iter()
        .map(|(name, _, _)| name.len())
        .max()
        .unwrap_or(0)
        .max("total".len());
    println!("\n{title}");
    for (name, elapsed, outcome) in &results {
        let mark = if outcome.is_ok() { "ok" } else { "FAILED" };
        println!("  {name:width$}  {:>6.1} s  {mark}", elapsed.as_secs_f64());
    }
    println!("  {:width$}  {:>6.1} s", "total", total.as_secs_f64());

    write_job_summary(title, &results, total);

    let failed: Vec<&str> = results
        .iter()
        .filter(|(_, _, outcome)| outcome.is_err())
        .map(|(name, _, _)| *name)
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed: {}", failed.join(", ")))
    }
}

/// In GitHub Actions, the timings also go on the run's summary page, which is
/// where the job breakdown in docs/testing-and-ci.md is read from.
fn write_job_summary(
    title: &str,
    results: &[(&'static str, Duration, Result<()>)],
    total: Duration,
) {
    let mut table = format!("### {title}\n\n| Step | Seconds | Result |\n|---|---:|---|\n");
    for (name, elapsed, outcome) in results {
        let mark = if outcome.is_ok() { "ok" } else { "**failed**" };
        table.push_str(&format!(
            "| {name} | {:.1} | {mark} |\n",
            elapsed.as_secs_f64()
        ));
    }
    table.push_str(&format!("| total | {:.1} | |\n\n", total.as_secs_f64()));
    append_job_summary(&table);
}

/// Adds Markdown to the run's summary page. Does nothing outside GitHub
/// Actions.
pub(crate) fn append_job_summary(markdown: &str) {
    let Some(path) = env::var_os("GITHUB_STEP_SUMMARY") else {
        return;
    };
    // The summary is a convenience. Failing to write it must not fail the job.
    if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(path) {
        let _ = file.write_all(markdown.as_bytes());
    }
}
