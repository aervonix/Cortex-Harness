//! The level table, row by row, through the engine
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Levels").

use cortex_policy::{
    Access, Engine, NoSandbox, Outcome, Request, SandboxState, StartError,
};
use cortex_protocol::error::code;
use cortex_protocol::lease::{Effect, ExpiryKind, GrantorKind, Level, RuleAction};

use crate::Said::{Allow, Ask, Deny, ReadOnly};
use crate::{Said, at, engine, engine_with, parts, read, reason, said, shell, user, write};

fn network(engine: &Engine, host: Option<&str>) -> Outcome {
    engine.decide(&Request {
        tool: "fetch",
        default: RuleAction::Allow,
        accesses: &[Access::Network {
            host: host.map(str::to_owned),
        }],
    })
}

fn ext(engine: &Engine, tool: &str) -> Outcome {
    engine.decide(&Request {
        tool: "tool_call",
        default: RuleAction::Ask,
        accesses: &[Access::Ext {
            tool: tool.to_owned(),
        }],
    })
}

/// One row of the table: what each kind of call comes to at a level, with
/// one allow rule for a command (`make *`) so that "a command on a safe
/// list that may write" has a case.
fn row(level: Level) -> [Said; 10] {
    let engine = engine_with(|parts| {
        parts.level = level;
        parts.rules = vec![user(Effect::Exec, "make *", RuleAction::Allow)];
    });
    [
        said(&read(&engine, "/work/app/src/main.rs")),
        said(&read(&engine, "/etc/hosts")),
        said(&write(&engine, "/work/app/src/main.rs")),
        said(&write(&engine, "/etc/hosts")),
        said(&shell(&engine, "ls -la")),
        said(&shell(&engine, "make all")),
        said(&shell(&engine, "cargo build")),
        said(&shell(&engine, "cargo build $FLAGS")),
        said(&network(&engine, Some("example.com"))),
        said(&shell(&engine, "echo x > out.txt")),
    ]
}

#[test]
fn each_level_decides_as_its_row_of_the_table_says() {
    //            read   beyond  edit   beyond  r/o cmd   safe   other  unmodelled  network  redirect
    assert_eq!(
        row(Level::ReadOnly),
        [Allow, Ask, Deny, Deny, ReadOnly, Ask, Ask, Ask, Ask, Deny]
    );
    assert_eq!(
        row(Level::Supervised),
        [Allow, Ask, Allow, Ask, ReadOnly, Allow, Ask, Ask, Ask, Allow]
    );
    assert_eq!(
        row(Level::Autonomous),
        [Allow, Ask, Allow, Ask, ReadOnly, Allow, Allow, Allow, Ask, Allow]
    );
    assert_eq!(
        row(Level::Unrestricted),
        [Allow, Allow, Allow, Allow, ReadOnly, Allow, Allow, Allow, Allow, Allow]
    );
}

/// At `read-only` the level refuses writes, and no rule makes one happen.
#[test]
fn at_read_only_no_rule_makes_a_write_happen() {
    let engine = engine_with(|parts| {
        parts.level = Level::ReadOnly;
        parts.rules = vec![
            user(Effect::Write, "**", RuleAction::Allow),
            user(Effect::Write, "src/**", RuleAction::Allow),
            user(Effect::Exec, "*", RuleAction::Allow),
        ];
    });
    for file in ["/work/app/src/main.rs", "/work/app/notes.txt", "/tmp/x", "/etc/hosts"] {
        let outcome = write(&engine, file);
        assert_eq!(said(&outcome), Deny, "{file}");
        assert!(reason(&outcome).contains("`read-only`"), "{}", reason(&outcome));
    }
    assert_eq!(said(&shell(&engine, "echo x > src/main.rs")), Deny);
    // A command that may write is asked about, allowed by a rule or not,
    // and one that only reads still runs.
    assert_eq!(said(&shell(&engine, "cargo build")), Ask);
    assert_eq!(said(&shell(&engine, "ls")), ReadOnly);
    // Reads are as at any level.
    assert_eq!(said(&read(&engine, "/work/app/src/main.rs")), Allow);
}

/// Rules still settle what they match at the levels that ask least.
#[test]
fn a_deny_holds_at_every_level() {
    for level in [Level::ReadOnly, Level::Supervised, Level::Autonomous, Level::Unrestricted] {
        let engine = engine_with(|parts| {
            parts.level = level.clone();
            parts.rules = vec![
                user(Effect::Exec, "git push *", RuleAction::Deny),
                user(Effect::Read, "secrets/**", RuleAction::Deny),
                user(Effect::Write, "vendor/**", RuleAction::Deny),
                user(Effect::Network, "evil.example", RuleAction::Deny),
                user(Effect::Ext, "docs/delete", RuleAction::Deny),
            ];
        });
        assert_eq!(said(&shell(&engine, "git push origin")), Deny, "{level:?}");
        assert_eq!(said(&shell(&engine, "echo $(git push)")), Deny, "{level:?}");
        assert_eq!(said(&read(&engine, "/work/app/secrets/key")), Deny, "{level:?}");
        assert_eq!(said(&write(&engine, "/work/app/vendor/x.rs")), Deny, "{level:?}");
        assert_eq!(said(&network(&engine, Some("evil.example"))), Deny, "{level:?}");
        assert_eq!(said(&ext(&engine, "docs/delete")), Deny, "{level:?}");
        // The harness's own files are closed at every level, to a tool and
        // to a command that names them.
        let credentials = "/home/dev/.config/cortex-harness/credentials.toml";
        assert_eq!(said(&read(&engine, credentials)), Deny, "{level:?}");
        assert_eq!(said(&write(&engine, credentials)), Deny, "{level:?}");
        assert_eq!(
            said(&shell(&engine, &format!("cat {credentials}"))),
            Ask,
            "{level:?}"
        );
        assert_eq!(
            said(&shell(&engine, &format!("cp {credentials} /tmp/x"))),
            Ask,
            "{level:?}"
        );
    }
}

#[test]
fn an_ask_rule_is_asked_at_every_level() {
    for level in [Level::Supervised, Level::Autonomous, Level::Unrestricted] {
        let engine = engine_with(|parts| {
            parts.level = level.clone();
            parts.rules = vec![
                user(Effect::Exec, "git push *", RuleAction::Ask),
                user(Effect::Exec, "git *", RuleAction::Allow),
                user(Effect::Write, "migrations/**", RuleAction::Ask),
                user(Effect::Write, "**", RuleAction::Allow),
            ];
        });
        assert_eq!(said(&shell(&engine, "git push")), Ask, "{level:?}");
        assert_eq!(said(&write(&engine, "/work/app/migrations/1.sql")), Ask, "{level:?}");
        assert_eq!(said(&write(&engine, "/work/app/src/a.rs")), Allow, "{level:?}");
    }
}

#[test]
fn autonomous_is_refused_where_the_sandbox_does_not_enforce() {
    for why in [
        NoSandbox::NotBuilt,
        NoSandbox::UnsupportedSystem,
        NoSandbox::NoLandlock,
        NoSandbox::TurnedOff,
    ] {
        let mut wanted = parts();
        wanted.level = Level::Autonomous;
        wanted.sandbox = SandboxState::NotEnforcing(why);
        let Err(StartError::Level(error)) = Engine::start(wanted) else {
            panic!("an autonomous session started with no sandbox ({why:?})");
        };
        let data = error.to_error_data();
        assert_eq!(data.code, code::SANDBOX_REQUIRED);
        assert!(data.message.contains("`autonomous`"), "{}", data.message);
        assert!(data.message.contains("`supervised`"), "{}", data.message);
        assert!(data.message.contains(why.explain()), "{}", data.message);

        // The other levels start, and say plainly that nothing is confined.
        for level in [Level::ReadOnly, Level::Supervised, Level::Unrestricted] {
            let engine = engine_with(|parts| {
                parts.level = level.clone();
                parts.sandbox = SandboxState::NotEnforcing(why);
            });
            let notice = engine.sandbox_notice().expect("the user is told");
            assert!(notice.starts_with("sandbox: off."), "{notice}");
        }
    }
    // With an enforcing sandbox it starts, and nothing needs saying.
    let engine = at(Level::Autonomous);
    assert_eq!(engine.level(), &Level::Autonomous);
    assert_eq!(engine.sandbox_notice(), None);
    // `guided` waits for task leases, and an unknown level is refused.
    for level in [Level::Guided, Level::Unknown("godlike".to_owned())] {
        let mut wanted = parts();
        wanted.level = level;
        assert!(matches!(Engine::start(wanted), Err(StartError::Level(_))));
    }
}

#[test]
fn the_engine_holds_the_sessions_standing_lease() {
    let engine = engine_with(|parts| {
        parts.rules = vec![
            user(Effect::Exec, "cargo test *", RuleAction::Allow),
            crate::project(Effect::Exec, "make deploy", RuleAction::Allow),
            crate::project(Effect::Write, "vendor/**", RuleAction::Deny),
        ];
        parts.trusted = false;
    });
    let lease = engine.lease();
    assert_eq!(lease.id.as_str(), "l_1");
    assert_eq!(lease.grant.level, Level::Supervised);
    assert_eq!(lease.expires.kind, ExpiryKind::EndOfSession);
    assert_eq!(lease.granted_by.kind, GrantorKind::User);
    // Its rules are the rules in force: the project's allow, which waits
    // for trust, is not among them.
    let rules: Vec<String> = lease
        .grant
        .rules
        .iter()
        .map(|rule| format!("{} {} {}", rule.action.as_str(), rule.effect.as_str(), rule.pattern))
        .collect();
    assert_eq!(rules, ["allow exec cargo test *", "deny write vendor/**"]);
    assert_eq!(lease.grant.sandbox.write.roots, ["/work/app", "/tmp/cortex-s1"]);
    // And it reads back through the protocol's strict reader.
    let wire = serde_json::to_value(lease).unwrap();
    assert_eq!(&cortex_protocol::lease::Lease::from_wire(wire).unwrap(), lease);
}

#[test]
fn a_rule_that_cannot_be_compiled_stops_the_session_from_starting() {
    let mut wanted = parts();
    wanted.rules = vec![
        user(Effect::Exec, "cargo test *", RuleAction::Allow),
        user(Effect::Write, "src/[abc", RuleAction::Deny),
    ];
    let Err(StartError::Rules(errors)) = Engine::start(wanted) else {
        panic!("a session started with a deny rule dropped");
    };
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().contains("`deny write src/[abc`"), "{}", errors[0]);
}

#[test]
fn the_network_asks_unless_a_rule_names_the_host() {
    let engine = engine_with(|parts| {
        parts.rules = vec![
            user(Effect::Network, "docs.rs", RuleAction::Allow),
            user(Effect::Network, "*.internal", RuleAction::Deny),
        ];
    });
    assert_eq!(said(&network(&engine, Some("docs.rs"))), Allow);
    assert_eq!(said(&network(&engine, Some("DOCS.RS"))), Allow);
    assert_eq!(said(&network(&engine, Some("example.com"))), Ask);
    assert_eq!(said(&network(&engine, Some("db.internal"))), Deny);
    // A call that cannot say where it goes meets the deny.
    assert_eq!(said(&network(&engine, None)), Deny);
    let open = self::engine();
    assert_eq!(said(&network(&open, None)), Ask);
    // An approval would name the host, and for "any host" there is none.
    let asked = crate::question(network(&open, Some("Example.COM.")));
    assert_eq!(asked.lifts.len(), 1);
    assert_eq!(asked.lifts[0].pattern, "example.com");
    assert!(crate::question(network(&open, None)).lifts.is_empty());
}

#[test]
fn an_extension_tool_asks_unless_a_rule_names_it() {
    let engine = engine_with(|parts| {
        parts.rules = vec![
            user(Effect::Ext, "docs/*", RuleAction::Allow),
            user(Effect::Ext, "docs/delete", RuleAction::Deny),
        ];
    });
    assert_eq!(said(&ext(&engine, "docs/find")), Allow);
    assert_eq!(said(&ext(&engine, "docs/delete")), Deny);
    assert_eq!(said(&ext(&engine, "github/create_issue")), Ask);
    let asked = crate::question(ext(&engine, "github/create_issue"));
    assert_eq!(asked.lifts[0].effect, Effect::Ext);
    assert_eq!(asked.lifts[0].pattern, "github/create_issue");
    // Whatever else the tool's plan declares is judged too.
    let with_write = engine.decide(&Request {
        tool: "tool_call",
        default: RuleAction::Ask,
        accesses: &[
            Access::Ext {
                tool: "docs/find".to_owned(),
            },
            Access::Write {
                path: crate::path("/etc/hosts"),
            },
        ],
    });
    assert_eq!(said(&with_write), Ask);
    // At `unrestricted` an extension's tool runs, and a deny still holds.
    let open = engine_with(|parts| {
        parts.level = Level::Unrestricted;
        parts.rules = vec![user(Effect::Ext, "docs/delete", RuleAction::Deny)];
    });
    assert_eq!(said(&ext(&open, "github/create_issue")), Allow);
    assert_eq!(said(&ext(&open, "docs/delete")), Deny);
}

#[test]
fn a_call_that_declares_nothing_follows_its_tools_own_default() {
    let engine = engine();
    let call = |default| {
        said(&engine.decide(&Request {
            tool: "plan",
            default,
            accesses: &[],
        }))
    };
    assert_eq!(call(RuleAction::Allow), Allow);
    assert_eq!(call(RuleAction::Ask), Ask);
    assert_eq!(call(RuleAction::Deny), Deny);
    // The shell tool's default is `ask`, and a command the level decides
    // is not asked about because of it.
    assert_eq!(said(&shell(&engine, "ls")), ReadOnly);
}
