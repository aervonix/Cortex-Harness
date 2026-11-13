//! Measuring how long loading and the check for changed files take. Not a
//! check itself: the budgets are in `docs/performance-budgets.md`, and the
//! performance tier checks them. This prints the numbers that page records.
//!
//! ```text
//! cargo test --release -p cortex-config --test it -- --ignored --nocapture measure
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use cortex_base::FakeClock;
use cortex_config::{LayerKind, LiveConfig, Override};

use crate::Machine;

const USER: &str = r#"config_version = 1
profile = "review"

[model]
main = "claude-opus-5-5"
fast = "claude-haiku-5-5"
effort = "medium"
speed = "fast"

[model.roles.fast]
speed = "standard"

[providers.local]
base_url = "http://localhost:11434"
first_event_timeout = "5m"
headers = { X-Team = "tools", Authorization = { env = "LOCAL_TOKEN" } }

[providers.work]
base_url = "https://llm.example.test/v1"
key_env = "WORK_KEY"

[retry]
attempts = 3
max_wait = "5m"

[sandbox]
protect = ["~/.ssh", "~/.aws"]

[[permissions.rules]]
effect = "exec"
match = "cargo test *"
action = "allow"

[[permissions.rules]]
effect = "exec"
match = "cargo build *"
action = "allow"

[[permissions.rules]]
effect = "write"
match = "**/.env*"
action = "deny"

[limits.day]
cost = 20.0

[limits.session]
cost = 5.0
requests = 400

[profiles.review]
model.effort = "high"
permissions.mode = "read-only"

[profiles.quick]
model.main = "claude-haiku-5-5"

[tui]
theme = "dusk"
density = "comfortable"

[keys.normal]
"ctrl+p" = "palette"
"#;

const PROJECT: &str = r#"[model]
effort = "high"

[[permissions.rules]]
effect = "write"
match = "**/secrets/**"
action = "deny"

[[permissions.rules]]
effect = "exec"
match = "make *"
action = "allow"

[sandbox]
protect = ["deploy"]

[mcp.servers.docs]
command = "./tools/docs-server"

[profiles.review]
model.speed = "standard"
"#;

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

/// Four files, a profile, the environment and the command line.
fn typical(test: &str) -> Machine {
    let mut typical = Machine::new(test);
    typical
        .layer(
            LayerKind::Managed,
            "[managed]\nlocked = [\"sandbox.enabled\"]\n",
        )
        .layer(LayerKind::User, USER)
        .layer(LayerKind::Project, PROJECT)
        .layer(LayerKind::ProjectLocal, "[retry]\nattempts = 4\n");
    typical.options.overrides = vec![Override::parse("model.speed_wait=5s").unwrap()];
    typical.options.env.set("CORTEX_LOG", "info");
    typical
}

#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn measure_loading() {
    const RUNS: usize = 2000;

    let nothing = Machine::new("measure_nothing");
    let (none_median, none_max) = time(RUNS, || drop(nothing.load()));

    let typical = typical("measure_typical");
    let (typical_median, typical_max) = time(RUNS, || drop(typical.load()));

    println!("loading configuration, release build, {RUNS} runs each:");
    println!("  no files (four are looked for):     median {none_median:?}, slowest {none_max:?}");
    println!(
        "  four files, a profile, env and -c:  median {typical_median:?}, slowest {typical_max:?}"
    );
    let loaded = typical.load();
    println!(
        "  the typical set has {} effective settings and {} rules",
        loaded.effective().len(),
        loaded.settings.permissions.rules.len()
    );
}

/// The check for changed files that runs on focus, on a key press and at
/// each turn: a `stat` of each file loaded, after the project's files are
/// found again through the path resolver.
#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn measure_the_change_check() {
    const RUNS: usize = 2000;
    // A clock long after the files were written, so that they count as
    // settled and a check finds nothing to reload.
    let clock = || Arc::new(FakeClock::starting_at("2099-01-01T00:00:00Z").unwrap());

    let with_project = typical("measure_check_project");
    let mut live = LiveConfig::start(with_project.options.clone(), clock()).unwrap();
    assert!(live.check().is_none());
    let (project_median, project_max) = time(RUNS, || assert!(live.check().is_none()));

    let mut without = typical("measure_check_no_project");
    without.options.workspace = None;
    let mut live = LiveConfig::start(without.options.clone(), clock()).unwrap();
    assert!(live.check().is_none());
    let (bare_median, bare_max) = time(RUNS, || assert!(live.check().is_none()));

    println!("the change check, release build, {RUNS} runs each:");
    println!(
        "  four files, the project's found again: median {project_median:?}, slowest \
         {project_max:?}"
    );
    println!(
        "  two files, no project:                median {bare_median:?}, slowest {bare_max:?}"
    );
}
