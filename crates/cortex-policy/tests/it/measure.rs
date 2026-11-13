//! Measuring what a decision costs. Not a check: the budget it bears on is
//! "tool call arguments complete → read-only tool started" in
//! `docs/performance-budgets.md`, and this prints the numbers that page
//! records.
//!
//! ```text
//! cargo test --release -p cortex-policy --test it -- --ignored --nocapture measure
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use cortex_base::{PathOptions, PathResolver};
use cortex_policy::{Access, Engine, Request, default_sandbox};
use cortex_protocol::lease::{Effect, RuleAction};

use crate::{engine_with, parts, path, shell, user};

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn time(runs: usize, mut work: impl FnMut()) -> (Duration, Duration) {
    // Once unmeasured, so that the first run's page faults are not counted.
    work();
    let samples: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed()
        })
        .collect();
    let slowest = samples.iter().max().copied().unwrap_or_default();
    (median(samples), slowest)
}

/// Twenty rules, of the kinds a configuration has.
fn rules() -> Vec<cortex_policy::SourcedRule> {
    let mut rules = vec![
        user(Effect::Write, "**/.env*", RuleAction::Deny),
        user(Effect::Read, "secrets/**", RuleAction::Deny),
        user(Effect::Write, "migrations/**", RuleAction::Ask),
        user(Effect::Read, "~/notes/**", RuleAction::Allow),
        user(Effect::Exec, "git push --force*", RuleAction::Deny),
        user(Effect::Exec, "git push *", RuleAction::Ask),
        user(Effect::Network, "docs.rs", RuleAction::Allow),
        user(Effect::Ext, "docs/*", RuleAction::Allow),
    ];
    for command in [
        "cargo test *",
        "cargo build *",
        "cargo check *",
        "cargo clippy *",
        "cargo fmt *",
        "npm run *",
        "npm test *",
        "make *",
        "pytest *",
        "go test *",
        "just *",
        "rustfmt *",
    ] {
        rules.push(user(Effect::Exec, command, RuleAction::Allow));
    }
    rules
}

#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn measure_deciding() {
    const RUNS: usize = 20_000;
    let engine = engine_with(|parts| parts.rules = rules());

    let read = [Access::Read {
        path: path("/work/app/src/main.rs"),
    }];
    let (read_median, read_max) = time(RUNS, || {
        drop(engine.decide(&Request {
            tool: "read",
            default: RuleAction::Allow,
            accesses: &read,
        }));
    });
    let (one_median, one_max) = time(RUNS, || drop(shell(&engine, "cargo test --workspace")));
    let line = "cd ./crates/cortex-policy && grep -n 'fn decide' src/engine.rs | sort | head -20 && git status";
    let (line_median, line_max) = time(RUNS, || drop(shell(&engine, line)));
    let (refused_median, refused_max) = time(RUNS, || {
        drop(shell(&engine, "cargo test $(echo --workspace)"))
    });

    // The same line with what it names followed on a real file system.
    let scratch = cortex_testkit::scratch!("measure_deciding");
    scratch.file("crates/cortex-policy/src/engine.rs", "fn decide() {}");
    let resolver = PathResolver::new(scratch.path(), PathOptions::new()).unwrap();
    let workspace = resolver.root().clone();
    let mut real = parts();
    real.rules = rules();
    real.sandbox_policy = default_sandbox(&workspace, &[], &[]);
    real.workspace = workspace.clone();
    real.home = None;
    real.rules
        .retain(|rule| !rule.rule.pattern.starts_with('~'));
    real.locate = Arc::new(resolver);
    let real = Engine::start(real).unwrap();
    let accesses = [
        Access::Exec {
            command: line.to_owned(),
            cwd: workspace.clone(),
        },
        Access::Write { path: workspace },
    ];
    let (real_median, real_max) = time(RUNS, || {
        drop(real.decide(&Request {
            tool: "shell",
            default: RuleAction::Ask,
            accesses: &accesses,
        }));
    });
    let (start_median, start_max) = time(2000, || {
        drop(engine_with(|parts| parts.rules = rules()));
    });

    println!("deciding a planned call, release build, 20 rules, {RUNS} runs each:");
    println!(
        "  a read of a file in the workspace:        median {read_median:?}, slowest {read_max:?}"
    );
    println!(
        "  `cargo test --workspace` (an allow rule):  median {one_median:?}, slowest {one_max:?}"
    );
    println!(
        "  a line of five commands, paths as text:    median {line_median:?}, slowest {line_max:?}"
    );
    println!(
        "  the same, paths followed on disk:          median {real_median:?}, slowest {real_max:?}"
    );
    println!(
        "  a line that is not modelled:               median {refused_median:?}, slowest {refused_max:?}"
    );
    println!(
        "making the engine (compiling 20 rules):      median {start_median:?}, slowest {start_max:?}"
    );
}
