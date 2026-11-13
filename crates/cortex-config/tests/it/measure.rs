//! Measuring how long loading takes. Not a check: the budget is in
//! `docs/performance-budgets.md`, and the performance tier checks it. This
//! prints the numbers that page records.
//!
//! ```text
//! cargo test --release -p cortex-config --test it -- --ignored --nocapture measure
//! ```

use std::time::{Duration, Instant};

use cortex_config::{LayerKind, Override};

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

#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn measure_loading() {
    const RUNS: usize = 2000;

    let nothing = Machine::new("measure_nothing");
    let (none_median, none_max) = time(RUNS, || drop(nothing.load()));

    let mut typical = Machine::new("measure_typical");
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
