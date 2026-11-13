//! Every merge rule, the order of the layers, and profiles, as one table
//! (`docs/architecture/08-configuration.md`, "Layers", "Merge rules" and
//! "Profiles").

use cortex_config::{LayerKind, Override};

use crate::{Machine, layer_of, value};

/// One row: what each layer says, and what is in effect afterwards.
#[derive(Default)]
struct Case {
    name: &'static str,
    managed: &'static str,
    user: &'static str,
    project: &'static str,
    local: &'static str,
    /// `--profile`.
    profile: Option<&'static str>,
    /// Environment variables.
    env: &'static [(&'static str, &'static str)],
    /// `-c key=value` arguments.
    cli: &'static [&'static str],
    /// Key, the effective value as TOML, and the layer it came from.
    expect: &'static [(&'static str, &'static str, &'static str)],
}

const ATTEMPTS: &str = "retry.attempts";

/// The table. Each row is a rule or a step in the order of the layers.
const CASES: &[Case] = &[
    // ---- The order of the layers, one step at a time. ----
    Case {
        name: "nothing set: the built-in default",
        managed: "",
        user: "",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            (ATTEMPTS, "5", "default"),
            ("model.speed", "\"standard\"", "default"),
        ],
    },
    Case {
        name: "the managed file over the defaults",
        managed: "[retry]\nattempts = 2",
        user: "",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[(ATTEMPTS, "2", "managed")],
    },
    Case {
        name: "the user's file over the managed file",
        managed: "[retry]\nattempts = 2",
        user: "[retry]\nattempts = 3",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[(ATTEMPTS, "3", "user")],
    },
    Case {
        name: "the project's file over the user's",
        managed: "[retry]\nattempts = 2",
        user: "[retry]\nattempts = 3",
        project: "[retry]\nattempts = 4",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[(ATTEMPTS, "4", "project")],
    },
    Case {
        name: "the project's local file over its committed one",
        managed: "[retry]\nattempts = 2",
        user: "[retry]\nattempts = 3",
        project: "[retry]\nattempts = 4",
        local: "[retry]\nattempts = 6",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[(ATTEMPTS, "6", "project-local")],
    },
    Case {
        name: "a profile over every file layer",
        managed: "[retry]\nattempts = 2",
        user: "[retry]\nattempts = 3\n[profiles.patient.retry]\nattempts = 7",
        project: "[retry]\nattempts = 4",
        local: "[retry]\nattempts = 6",
        profile: Some("patient"),
        env: &[],
        cli: &[],
        expect: &[(ATTEMPTS, "7", "profile")],
    },
    Case {
        name: "the command line over a profile",
        managed: "",
        user: "[retry]\nattempts = 3\n[profiles.patient.retry]\nattempts = 7",
        project: "",
        local: "[retry]\nattempts = 6",
        profile: Some("patient"),
        env: &[],
        cli: &["retry.attempts=8"],
        expect: &[(ATTEMPTS, "8", "command-line")],
    },
    Case {
        name: "the environment over a profile, and the command line over the environment",
        managed: "",
        user: "[log]\nlevel = \"warn\"\n[profiles.loud.log]\nlevel = \"info\"",
        project: "",
        local: "",
        profile: Some("loud"),
        env: &[("CORTEX_LOG", "debug")],
        cli: &[],
        expect: &[("log.level", "\"debug\"", "environment")],
    },
    Case {
        name: "the command line over the environment",
        managed: "",
        user: "[log]\nlevel = \"warn\"",
        project: "",
        local: "",
        profile: None,
        env: &[("CORTEX_LOG", "debug")],
        cli: &["log.level=trace"],
        expect: &[("log.level", "\"trace\"", "command-line")],
    },
    Case {
        name: "a later command-line setting over an earlier one",
        managed: "",
        user: "",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &["retry.attempts=8", "retry.attempts=9"],
        expect: &[(ATTEMPTS, "9", "command-line")],
    },
    // ---- The merge rules. ----
    Case {
        name: "tables merge key by key",
        managed: "",
        user: "[model]\nmain = \"big\"\neffort = \"low\"\n[model.roles.fast]\nspeed = \"fast\"",
        project: "[model]\neffort = \"high\"\n[model.roles.fast]\neffort = \"low\"",
        local: "[model.roles.judge]\neffort = \"max\"",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            ("model.main", "\"big\"", "user"),
            ("model.effort", "\"high\"", "project"),
            ("model.roles.fast.speed", "\"fast\"", "user"),
            ("model.roles.fast.effort", "\"low\"", "project"),
            ("model.roles.judge.effort", "\"max\"", "project-local"),
            // A key no layer names keeps its default.
            ("model.speed", "\"standard\"", "default"),
        ],
    },
    Case {
        name: "a scalar replaces",
        managed: "",
        user: "[model]\nspeed_wait = \"5s\"\nspeed_fallback = false",
        project: "[model]\nspeed_wait = \"1m\"",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            ("model.speed_wait", "\"1m\"", "project"),
            ("model.speed_fallback", "false", "user"),
        ],
    },
    Case {
        name: "a list replaces: a layer that sets a list sets all of it",
        managed: "[tui]\npanels = [\"plan\", \"review\"]",
        user: "[tui]\npanels = [\"usage\"]",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[("tui.panels", "[\"usage\"]", "user")],
    },
    Case {
        name: "a named collection takes an entry from each layer, and merges one entry's keys",
        managed: "[providers.shared]\nbase_url = \"https://llm.example.test\"",
        user: "[providers.shared]\nkey_env = \"SHARED_KEY\"\n\
               [providers.local]\nbase_url = \"http://localhost:11434\"\n\
               [providers.old]\nenabled = false",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            (
                "providers.shared.base_url",
                "\"https://llm.example.test\"",
                "managed",
            ),
            ("providers.shared.key_env", "\"SHARED_KEY\"", "user"),
            (
                "providers.local.base_url",
                "\"http://localhost:11434\"",
                "user",
            ),
            // An entry is removed by turning it off, not by leaving it out.
            ("providers.old.enabled", "false", "user"),
        ],
    },
    Case {
        name: "permission rules from every layer are joined",
        managed: "[[permissions.rules]]\neffect = \"write\"\nmatch = \"/etc/**\"\naction = \"deny\"",
        user: "[[permissions.rules]]\neffect = \"exec\"\nmatch = \"cargo test *\"\naction = \"allow\"",
        project: "[[permissions.rules]]\neffect = \"write\"\nmatch = \"**/.env*\"\naction = \"deny\"",
        local: "[[permissions.rules]]\neffect = \"exec\"\nmatch = \"rm *\"\naction = \"ask\"",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[(
            "permissions.rules",
            "[{ action = \"deny\", effect = \"write\", match = \"/etc/**\" }, \
             { action = \"allow\", effect = \"exec\", match = \"cargo test *\" }, \
             { action = \"deny\", effect = \"write\", match = \"**/.env*\" }, \
             { action = \"ask\", effect = \"exec\", match = \"rm *\" }]",
            "default",
        )],
    },
    Case {
        name: "the sandbox's protected paths are joined too, so no layer can drop one",
        managed: "[sandbox]\nprotect = [\"/srv/keys\"]",
        user: "[sandbox]\nprotect = [\"~/.ssh\"]\nwrite = [\"~/scratch\"]",
        project: "[sandbox]\nprotect = []",
        local: "[sandbox]\nprotect = [\"deploy\"]",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            (
                "sandbox.protect",
                "[\"/srv/keys\", \"~/.ssh\", \"deploy\"]",
                "default",
            ),
            ("sandbox.write", "[\"~/scratch\"]", "default"),
        ],
    },
    // ---- Locks. ----
    Case {
        name: "a locked key ignores every layer above the managed file",
        managed: "[managed]\nlocked = [\"permissions.mode\"]\n[permissions]\nmode = \"read-only\"",
        user: "[permissions]\nmode = \"supervised\"\n[profiles.open.permissions]\nmode = \"autonomous\"",
        project: "[permissions]\nmode = \"supervised\"",
        local: "",
        profile: Some("open"),
        env: &[],
        cli: &["permissions.mode=unrestricted"],
        expect: &[("permissions.mode", "\"read-only\"", "managed")],
    },
    Case {
        name: "a lock on a table locks every key in it, set by the managed file or not",
        managed: "[managed]\nlocked = [\"retry\", \"log.level\"]\n[retry]\nmax_wait = \"30s\"",
        user: "[retry]\nattempts = 9\nmax_wait = \"10m\"\n[log]\nlevel = \"trace\"\n[model]\nmain = \"big\"",
        project: "",
        local: "",
        profile: None,
        env: &[("CORTEX_LOG", "debug")],
        cli: &["retry.attempts=1"],
        expect: &[
            (ATTEMPTS, "5", "default"),
            ("retry.max_wait", "\"30s\"", "managed"),
            ("log.level", "(not set)", "default"),
            // What is not locked is the user's as usual.
            ("model.main", "\"big\"", "user"),
        ],
    },
    Case {
        name: "a locked list takes entries from the managed file only",
        managed: "[managed]\nlocked = [\"permissions.rules\"]\n\
                  [[permissions.rules]]\neffect = \"exec\"\nmatch = \"*\"\naction = \"ask\"",
        user: "[[permissions.rules]]\neffect = \"exec\"\nmatch = \"*\"\naction = \"allow\"",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[(
            "permissions.rules",
            "[{ action = \"ask\", effect = \"exec\", match = \"*\" }]",
            "default",
        )],
    },
    // ---- Profiles. ----
    Case {
        name: "a profile is selected by the `profile` key",
        managed: "",
        user: "profile = \"review\"\n[model]\nmain = \"big\"\n\
               [profiles.review]\nmodel.main = \"careful\"\nmodel.effort = \"high\"\n\
               [profiles.quick]\nmodel.main = \"small\"",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            ("profile", "\"review\"", "user"),
            ("model.main", "\"careful\"", "profile"),
            ("model.effort", "\"high\"", "profile"),
        ],
    },
    Case {
        name: "the --profile flag wins over the `profile` key",
        managed: "",
        user: "profile = \"review\"\n\
               [profiles.review]\nmodel.main = \"careful\"\n\
               [profiles.quick]\nmodel.main = \"small\"",
        project: "",
        local: "",
        profile: Some("quick"),
        env: &[],
        cli: &[],
        expect: &[
            ("profile", "\"quick\"", "command-line"),
            ("model.main", "\"small\"", "profile"),
        ],
    },
    Case {
        name: "a profile defined in several layers merges like any table, higher layer last",
        managed: "[profiles.review.retry]\nattempts = 2\nmax_wait = \"1m\"",
        user: "profile = \"review\"\n[profiles.review]\nmodel.main = \"careful\"\nretry.attempts = 3",
        project: "[profiles.review]\nmodel.effort = \"max\"\nmodel.main = \"theirs\"",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            ("profile", "\"review\"", "user"),
            ("retry.max_wait", "\"1m\"", "profile"),
            (ATTEMPTS, "3", "profile"),
            ("model.main", "\"theirs\"", "profile"),
            ("model.effort", "\"max\"", "profile"),
        ],
    },
    Case {
        name: "a profile that is not selected changes nothing",
        managed: "",
        user: "[model]\nmain = \"big\"\n[profiles.quick]\nmodel.main = \"small\"",
        project: "",
        local: "",
        profile: None,
        env: &[],
        cli: &[],
        expect: &[
            ("model.main", "\"big\"", "user"),
            ("profile", "(not set)", "default"),
        ],
    },
    Case {
        name: "a profile can select a mode along with anything else",
        managed: "",
        user: "[profiles.review]\npermissions.mode = \"read-only\"\nmodel.speed = \"fast\"\n\
               sandbox.protect = [\"notes\"]",
        project: "",
        local: "",
        profile: Some("review"),
        env: &[],
        cli: &[],
        expect: &[
            ("permissions.mode", "\"read-only\"", "profile"),
            ("model.speed", "\"fast\"", "profile"),
            ("sandbox.protect", "[\"notes\"]", "default"),
        ],
    },
];

#[test]
fn every_merge_rule_layer_and_profile() {
    for case in CASES {
        let mut machine = Machine::new("layers_table");
        machine
            .layer(LayerKind::Managed, case.managed)
            .layer(LayerKind::User, case.user)
            .layer(LayerKind::Project, case.project)
            .layer(LayerKind::ProjectLocal, case.local);
        machine.options.profile = case.profile.map(str::to_owned);
        for (name, value) in case.env {
            machine.options.env.set(name, *value);
        }
        machine.options.overrides = case
            .cli
            .iter()
            .map(|argument| Override::parse(argument).unwrap())
            .collect();

        let loaded = machine.load();
        for (key, expected, layer) in case.expect {
            assert_eq!(
                value(&loaded, key),
                *expected,
                "{}: the value of {key}",
                case.name
            );
            assert_eq!(
                layer_of(&loaded, key),
                *layer,
                "{}: where {key} came from",
                case.name
            );
        }
    }
}

#[test]
fn every_layer_is_in_the_table() {
    // The rows above name each of the eight layers as the source of a
    // value, so none is untested.
    let named: Vec<&str> = CASES
        .iter()
        .flat_map(|case| case.expect.iter().map(|(_, _, layer)| *layer))
        .collect();
    for layer in LayerKind::ALL {
        assert!(named.contains(&layer.as_str()), "no row for {layer}");
    }
}

#[test]
fn each_joined_entry_remembers_its_layer() {
    let machine = Machine::new("layers_rule_origins");
    machine
        .layer(
            LayerKind::Managed,
            "[[permissions.rules]]\neffect = \"write\"\nmatch = \"/etc/**\"\naction = \"deny\"\n",
        )
        .layer(
            LayerKind::User,
            "# mine\n[[permissions.rules]]\neffect = \"exec\"\nmatch = \"cargo test *\"\n\
             action = \"allow\"\n\n[[permissions.rules]]\neffect = \"read\"\nmatch = \"~/notes/**\"\n\
             action = \"allow\"\n",
        )
        .layer(
            LayerKind::Project,
            "[[permissions.rules]]\neffect = \"write\"\nmatch = \"**/.env*\"\naction = \"deny\"\n",
        );
    let loaded = machine.load();
    let placed: Vec<(String, &str, Option<u32>)> = loaded
        .rules()
        .into_iter()
        .map(|placed| {
            (
                placed.rule.pattern,
                placed.origin.layer.as_str(),
                placed.origin.line,
            )
        })
        .collect();
    assert_eq!(
        placed,
        [
            ("/etc/**".to_owned(), "managed", Some(1)),
            ("cargo test *".to_owned(), "user", Some(2)),
            ("~/notes/**".to_owned(), "user", Some(7)),
            ("**/.env*".to_owned(), "project", Some(1)),
        ]
    );
    assert_eq!(loaded.settings.permissions.rules.len(), 4);
}

#[test]
fn a_lock_is_reported_where_it_bites() {
    let machine = Machine::new("layers_lock_notes");
    machine
        .layer(
            LayerKind::Managed,
            "[managed]\nlocked = [\"permissions.mode\", \"not.a.key\", \"bad key\"]\n\
             [permissions]\nmode = \"read-only\"\n",
        )
        .layer(LayerKind::User, "[permissions]\nmode = \"supervised\"\n");
    let loaded = machine.load();
    assert_eq!(
        machine.said(&loaded),
        [
            "warning: ~/managed/config.toml:2: managed.locked: `not.a.key` names no key this \
             version knows; the lock is kept",
            "warning: ~/managed/config.toml:2: managed.locked: `bad key` is not a key: parts are \
             separated by dots; this lock is ignored",
            "note: ~/cortex/config.toml:2: permissions.mode: locked by the managed \
             configuration; this value is not used",
        ]
    );
    assert_eq!(value(&loaded, "permissions.mode"), "\"read-only\"");
}

#[test]
fn a_profile_that_does_not_exist_is_an_error_that_lists_the_ones_that_do() {
    let mut machine = Machine::new("layers_missing_profile");
    machine.layer(
        LayerKind::User,
        "profile = \"reveiw\"\n[profiles.review]\nmodel.main = \"a\"\n[profiles.quick]\nmodel.main = \"b\"\n",
    );
    assert_eq!(
        machine.failure(),
        [
            "error: ~/cortex/config.toml:1: profile: there is no profile named `reveiw`. The \
          profiles are: quick, review"
        ]
    );

    machine.options.profile = Some("nightly".to_owned());
    assert_eq!(
        machine.failure(),
        [
            "error: command-line: profile: there is no profile named `nightly`. The profiles are: \
          quick, review"
        ]
    );

    machine.layer(LayerKind::User, "[model]\nmain = \"a\"\n");
    assert_eq!(
        machine.failure(),
        [
            "error: command-line: profile: there is no profile named `nightly`: no profile is \
          defined"
        ]
    );
}
