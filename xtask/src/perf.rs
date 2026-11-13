//! `cargo xtask perf`: the TUI's budgets measured on the demo, by the method
//! of `docs/measurements/tui-language-spike.md`.
//!
//! It builds the demo with optimisation (the example `demo` of
//! `cortex-term`), then runs the ignored measurement test of `cortex-term`'s
//! integration binary, also optimised, which starts the demo in a
//! pseudo-terminal (ConPTY on Windows) and prints a table: start to first
//! frame, key to frame when idle and while streaming (and the demo's own
//! share of it), frame cost, frame intervals and bytes per frame. What is
//! measured, and how, is that test's documentation
//! (`crates/cortex-term/tests/it/perf.rs`).
//!
//! The rest of the performance budgets (start-up of the real binary, memory,
//! binary size) have commands of their own: `startup` and `size`.

use std::path::PathBuf;

use crate::Result;
use crate::{outside, steps, workspace};

/// What `perf` was asked for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Request {
    runs: Option<u64>,
    seconds: Option<u64>,
    json: Option<String>,
}

fn parse(arguments: &[String]) -> Result<Request> {
    let mut request = Request::default();
    let mut rest = arguments.iter();
    let number = |value: Option<&String>, flag: &str| -> Result<u64> {
        value
            .and_then(|value| value.parse().ok())
            .filter(|number| *number > 0)
            .ok_or_else(|| format!("`{flag}` takes a number above zero"))
    };
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--runs" => request.runs = Some(number(rest.next(), "--runs")?),
            "--seconds" => request.seconds = Some(number(rest.next(), "--seconds")?),
            "--json" => request.json = Some(rest.next().ok_or("`--json` takes a path")?.clone()),
            other => {
                return Err(format!(
                    "unknown argument `{other}`\nusage: cargo xtask perf [--runs N] [--seconds N] [--json PATH]"
                ));
            }
        }
    }
    Ok(request)
}

/// Where cargo puts the optimised demo.
fn demo_binary() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| workspace::root().join("target"), PathBuf::from);
    target
        .join("release")
        .join("examples")
        .join(format!("demo{}", std::env::consts::EXE_SUFFIX))
}

/// `cargo xtask perf [--runs N] [--seconds N] [--json PATH]`.
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let request = parse(arguments)?;
    steps::cargo(&[
        "build",
        "--locked",
        "--release",
        "-p",
        "cortex-term",
        "--example",
        "demo",
        "--features",
        "demo",
    ])?;
    let demo = demo_binary();
    let demo = demo
        .to_str()
        .ok_or("the target directory's path is not Unicode")?;
    let runs = request.runs.map(|runs| runs.to_string());
    let seconds = request.seconds.map(|seconds| seconds.to_string());
    let mut variables = vec![("CORTEX_PERF_DEMO", demo)];
    if let Some(runs) = &runs {
        variables.push(("CORTEX_PERF_RUNS", runs));
    }
    if let Some(seconds) = &seconds {
        variables.push(("CORTEX_PERF_SECONDS", seconds));
    }
    if let Some(json) = &request.json {
        variables.push(("CORTEX_PERF_JSON", json));
    }
    outside::run_ignored(
        &["--release", "--package", "cortex-term", "--test", "it"],
        "perf::perf_the_demo_against_the_budgets",
        &variables,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&[]).unwrap(), Request::default());
        assert_eq!(
            parse(&strings(&[
                "--runs",
                "5",
                "--seconds",
                "3",
                "--json",
                "out.json"
            ]))
            .unwrap(),
            Request {
                runs: Some(5),
                seconds: Some(3),
                json: Some("out.json".to_owned()),
            }
        );
        assert!(parse(&strings(&["--runs", "0"])).is_err());
        assert!(parse(&strings(&["--runs"])).is_err());
        assert!(parse(&strings(&["--frames"])).is_err());
    }
}
