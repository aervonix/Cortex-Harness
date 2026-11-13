//! Where each effective value came from
//! (`docs/architecture/08-configuration.md`, "Provenance"): what
//! `cortex config show --origin` and `--layer` print, as snapshots.

use cortex_config::{KeyPath, LayerKind, Override};

use crate::{Machine, assert_snapshot};

const MANAGED: &str = r#"# Set by the administrator of this machine.
[managed]
locked = ["sandbox.enabled"]

[sandbox]
enabled = true
protect = ["/srv/keys"]

[[permissions.rules]]
effect = "write"
match = "/etc/**"
action = "deny"
"#;

const USER: &str = r#"config_version = 1
profile = "review"

[model]
main = "claude-opus-5-5"
fast = "claude-haiku-5-5"
effort = "medium"

[model.roles.fast]
speed = "standard"

[providers.local]
base_url = "http://localhost:11434"
first_event_timeout = "5m"
headers = { X-Team = "tools", Authorization = { env = "LOCAL_TOKEN" } }

[retry]
attempts = 3

[sandbox]
enabled = false

[[permissions.rules]]
effect = "exec"
match = "cargo test *"
action = "allow"

[limits.day]
cost = 20.0

[profiles.review]
model.effort = "high"
permissions.mode = "read-only"

[tui]
theme = "dusk"
"#;

const PROJECT: &str = r#"[model]
main = "claude-sonnet-5-5"

[[permissions.rules]]
effect = "write"
match = "**/.env*"
action = "deny"

[profiles.review]
model.speed = "fast"
"#;

const LOCAL: &str = r#"[retry]
max_wait = "5m"
"#;

fn machine(test: &str) -> Machine {
    let mut machine = Machine::new(test);
    machine
        .layer(LayerKind::Managed, MANAGED)
        .layer(LayerKind::User, USER)
        .layer(LayerKind::Project, PROJECT)
        .layer(LayerKind::ProjectLocal, LOCAL);
    machine
        .options
        .env
        .set("CORTEX_LOG", "info,cortex_llm=debug");
    machine.options.overrides = vec![
        Override::parse("model.speed_wait=5s").unwrap(),
        Override {
            key: "model.judge".to_owned(),
            value: "claude-opus-5-5".to_owned(),
            flag: Some("--judge-model".to_owned()),
        },
    ];
    machine
}

#[test]
fn show_with_origins_says_where_every_value_came_from() {
    let machine = machine("provenance_show");
    let loaded = machine.load();
    assert_snapshot("show-origin.txt", &loaded.show(machine.env(), true));
    assert_snapshot("show.txt", &loaded.show(machine.env(), false));
}

#[test]
fn each_layer_can_be_shown_as_it_was_read() {
    let machine = machine("provenance_layers");
    let loaded = machine.load();
    let mut all = String::new();
    for layer in LayerKind::ALL {
        all.push_str(&format!("## {layer}\n"));
        all.push_str(&loaded.show_layer(layer, machine.env()));
        all.push('\n');
    }
    assert_snapshot("show-layers.txt", &all);
}

#[test]
fn one_origin_reads_as_the_design_page_shows_it() {
    let machine = machine("provenance_one");
    let loaded = machine.load();
    let origin = |key: &str| {
        loaded
            .origin(&KeyPath::parse(key).unwrap())
            .render(machine.env())
    };
    assert_eq!(origin("model.fast"), "user: ~/cortex/config.toml:6");
    assert_eq!(
        origin("model.main"),
        "project: ~/work/app/.cortex/config.toml:2"
    );
    assert_eq!(
        origin("retry.max_wait"),
        "project-local: ~/work/app/.cortex/config.local.toml:2"
    );
    assert_eq!(
        origin("model.effort"),
        "profile review (user: ~/cortex/config.toml:32)"
    );
    assert_eq!(
        origin("model.speed"),
        "profile review (project: ~/work/app/.cortex/config.toml:10)"
    );
    assert_eq!(
        origin("sandbox.enabled"),
        "managed: ~/managed/config.toml:6"
    );
    assert_eq!(origin("log.level"), "environment: CORTEX_LOG");
    assert_eq!(
        origin("model.speed_wait"),
        "command-line: -c model.speed_wait"
    );
    assert_eq!(origin("model.judge"), "command-line: --judge-model");
    assert_eq!(origin("retry.backoff_cap"), "default");
    assert_eq!(origin("profile"), "user: ~/cortex/config.toml:2");
}
