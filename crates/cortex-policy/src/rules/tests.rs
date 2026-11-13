//! Rule evaluation: precedence, sources and trust, the built-in protection,
//! and matching on Windows.

use cortex_base::{PathStyle, Rng, SeededRng};
use cortex_testkit::cases::{self, ensure};

use super::*;
use crate::command::{Analysis, analyse};

fn rule(effect: Effect, pattern: &str, action: RuleAction) -> Rule {
    Rule {
        effect,
        pattern: pattern.to_owned(),
        action,
    }
}

fn user(effect: Effect, pattern: &str, action: RuleAction) -> SourcedRule {
    SourcedRule::new(rule(effect, pattern, action), RuleSource::User)
}

fn posix(path: &str) -> AbsPath {
    AbsPath::parse(path, PathStyle::Posix).unwrap()
}

fn set_with(rules: Vec<SourcedRule>, trusted: bool, protected: Protected) -> RuleSet {
    let workspace = posix("/work/app");
    let home = posix("/users/dev");
    RuleSet::compile(
        rules,
        RuleContext {
            workspace: &workspace,
            home: Some(&home),
            trusted,
        },
        protected,
    )
    .unwrap()
}

fn set(rules: Vec<SourcedRule>) -> RuleSet {
    set_with(rules, true, Protected::none())
}

fn action(hit: Option<Hit<'_>>) -> Option<RuleAction> {
    hit.map(|hit| hit.action)
}

use RuleAction::{Allow, Ask, Deny};

#[test]
fn deny_beats_ask_beats_allow_whatever_the_order() {
    let rules = [
        user(Effect::Write, "src/**", Allow),
        user(Effect::Write, "src/generated/**", Ask),
        user(Effect::Write, "**/.env*", Deny),
    ];
    // Every order of the three rules gives the same answers.
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let set = set(order.iter().map(|&index| rules[index].clone()).collect());
        let write = |path: &str| action(set.path(&Effect::Write, &posix(path)));
        assert_eq!(write("/work/app/src/main.rs"), Some(Allow), "{order:?}");
        assert_eq!(
            write("/work/app/src/generated/a.rs"),
            Some(Ask),
            "{order:?}"
        );
        assert_eq!(write("/work/app/src/.env"), Some(Deny), "{order:?}");
        assert_eq!(
            write("/work/app/src/generated/.env.local"),
            Some(Deny),
            "{order:?}"
        );
        assert_eq!(write("/work/app/docs/a.md"), None, "{order:?}");
        // A rule is for one effect.
        assert_eq!(
            action(set.path(&Effect::Read, &posix("/work/app/src/.env"))),
            None
        );
    }
}

/// The same pattern is wide in a rule that holds back and narrow in a
/// rule that grants: `**/…` is the whole machine for a deny or an ask, and
/// the workspace for an allow.
#[test]
fn an_allow_that_starts_anywhere_is_an_allow_in_the_workspace() {
    for effect in [Effect::Read, Effect::Write] {
        let allows = set(vec![
            user(effect.clone(), "**/*.md", Allow),
            user(effect.clone(), "**", Allow),
        ]);
        for inside in [
            "/work/app/README.md",
            "/work/app/docs/a/b.md",
            "/work/app/x",
        ] {
            assert_eq!(action(allows.path(&effect, &posix(inside))), Some(Allow));
        }
        for outside in [
            "/users/dev/notes.md",
            "/users/dev/.ssh/id_rsa",
            "/users/dev/.bashrc",
            "/etc/passwd",
            "/work/app2/README.md",
            "/work",
        ] {
            assert_eq!(
                action(allows.path(&effect, &posix(outside))),
                None,
                "{outside}"
            );
        }
        // A rule that holds back, written the same way, holds everywhere.
        for held in [Deny, Ask] {
            let holds = set(vec![user(effect.clone(), "**/*.md", held)]);
            for path in ["/work/app/README.md", "/users/dev/notes.md", "/etc/x.md"] {
                assert_eq!(action(holds.path(&effect, &posix(path))), Some(held));
            }
        }
        // An allow that says where is an allow there.
        let named = set(vec![
            user(effect.clone(), "~/notes/**", Allow),
            user(effect.clone(), "/srv/data/**", Allow),
        ]);
        assert_eq!(
            action(named.path(&effect, &posix("/users/dev/notes/a.md"))),
            Some(Allow)
        );
        assert_eq!(
            action(named.path(&effect, &posix("/srv/data/x"))),
            Some(Allow)
        );
    }
}

#[test]
fn a_hit_says_which_rule_and_whose() {
    let set = set(vec![
        user(Effect::Write, "**/.env*", Deny),
        SourcedRule::new(
            rule(Effect::Read, "~/notes/**", Allow),
            RuleSource::CommandLine,
        ),
    ]);
    let hit = set.path(&Effect::Write, &posix("/work/app/.env")).unwrap();
    assert_eq!(
        hit.describe(),
        "the rule `deny write **/.env*` from your configuration"
    );
    let hit = set
        .path(&Effect::Read, &posix("/users/dev/notes/a.md"))
        .unwrap();
    assert_eq!(
        hit.describe(),
        "the rule `allow read ~/notes/**` from the command line"
    );
}

#[test]
fn a_rule_that_cannot_be_read_refuses_the_whole_set() {
    let workspace = posix("/work/app");
    let context = RuleContext {
        workspace: &workspace,
        home: None,
        trusted: true,
    };
    let errors = RuleSet::compile(
        vec![
            user(Effect::Write, "src/**", Allow),
            user(Effect::Write, "src/[abc", Deny),
            user(Effect::Exec, "", Deny),
            user(Effect::Read, "~/notes/**", Allow),
            user(Effect::Unknown("teleport".to_owned()), "*", Deny),
            user(Effect::Exec, "* --help", Allow),
            user(Effect::Network, "[", Ask),
        ],
        context,
        Protected::none(),
    )
    .unwrap_err();
    let patterns: Vec<&str> = errors
        .iter()
        .map(|error| error.rule.rule.pattern.as_str())
        .collect();
    assert_eq!(
        patterns,
        ["src/[abc", "", "~/notes/**", "*", "* --help", "["]
    );
    assert!(
        errors[4]
            .to_string()
            .contains("`allow exec * --help` from your configuration")
    );
    assert!(errors[3].problem.contains("teleport"));
}

// ------------------------------------------------------------------ trust

/// A project's rules can tighten at once, and loosen only once the user has
/// trusted the project.
#[test]
fn a_projects_allow_waits_for_trust_and_its_deny_does_not() {
    let rules = || {
        vec![
            SourcedRule::new(
                rule(Effect::Exec, "make deploy", Allow),
                RuleSource::Project,
            ),
            SourcedRule::new(rule(Effect::Write, "~/.ssh/**", Allow), RuleSource::Project),
            SourcedRule::new(rule(Effect::Write, "vendor/**", Deny), RuleSource::Project),
            SourcedRule::new(rule(Effect::Exec, "git push *", Ask), RuleSource::Project),
            SourcedRule::new(rule(Effect::Exec, "npm test", Allow), RuleSource::Learned),
            user(Effect::Exec, "cargo test *", Allow),
            SourcedRule::new(
                rule(Effect::Exec, "cargo build *", Allow),
                RuleSource::Approved,
            ),
            SourcedRule::new(rule(Effect::Exec, "ls *", Allow), RuleSource::Session),
            SourcedRule::new(rule(Effect::Exec, "id", Allow), RuleSource::Managed),
        ]
    };
    let part = |command: &str| match analyse(command, ShellSyntax::Posix) {
        Analysis::Parts(mut parts) => parts.remove(0),
        Analysis::NotModelled { why } => panic!("{why}"),
    };
    let allowed = |set: &RuleSet, command: &str| {
        set.exec_allowance(&part(command), ShellSyntax::Posix)
            .is_some()
    };

    let untrusted = set_with(rules(), false, Protected::none());
    // The project's and the learned allows are set aside, and listed.
    assert!(!allowed(&untrusted, "make deploy"));
    assert!(!allowed(&untrusted, "npm test"));
    assert_eq!(
        action(untrusted.path(&Effect::Write, &posix("/users/dev/.ssh/authorized_keys"))),
        None
    );
    let withheld: Vec<String> = untrusted.withheld().iter().map(SourcedRule::show).collect();
    assert_eq!(
        withheld,
        [
            "allow exec make deploy",
            "allow write ~/.ssh/**",
            "allow exec npm test"
        ]
    );
    // Its deny and its ask are in force all the same.
    assert_eq!(
        action(untrusted.path(&Effect::Write, &posix("/work/app/vendor/x.rs"))),
        Some(Deny)
    );
    assert_eq!(
        action(untrusted.exec_restriction("git push origin", None, ShellSyntax::Posix)),
        Some(Ask)
    );
    // The user's own rules never wait: configuration, approvals, session.
    for command in ["cargo test", "cargo build --release", "ls -la", "id"] {
        assert!(allowed(&untrusted, command), "{command}");
    }

    let trusted = set_with(rules(), true, Protected::none());
    assert!(trusted.withheld().is_empty());
    assert!(allowed(&trusted, "make deploy"));
    assert!(allowed(&trusted, "npm test"));
    assert_eq!(
        action(trusted.path(&Effect::Write, &posix("/users/dev/.ssh/authorized_keys"))),
        Some(Allow)
    );
}

/// No layer can weaken another: what one denies or asks about, an allow
/// from any other does not open.
#[test]
fn an_allow_from_any_source_never_opens_what_another_source_holds_back() {
    let sources = [
        RuleSource::Managed,
        RuleSource::User,
        RuleSource::CommandLine,
        RuleSource::Project,
        RuleSource::Learned,
        RuleSource::Approved,
        RuleSource::Session,
        RuleSource::Lease,
    ];
    for holder in sources {
        for opener in sources {
            for held in [Deny, Ask] {
                let set = set(vec![
                    SourcedRule::new(rule(Effect::Write, "secrets/**", held), holder),
                    SourcedRule::new(rule(Effect::Write, "**", Allow), opener),
                    SourcedRule::new(rule(Effect::Exec, "git push *", held), holder),
                    SourcedRule::new(rule(Effect::Exec, "git *", Allow), opener),
                ]);
                assert_eq!(
                    action(set.path(&Effect::Write, &posix("/work/app/secrets/key"))),
                    Some(held),
                    "{holder:?} against {opener:?}"
                );
                assert_eq!(
                    action(set.exec_restriction("git push origin", None, ShellSyntax::Posix)),
                    Some(held),
                    "{holder:?} against {opener:?}"
                );
            }
        }
    }
}

// ------------------------------------------------------------- built-ins

#[test]
fn the_harnesss_own_files_are_closed_whatever_the_rules_say() {
    let protected = Protected::none()
        .closed(posix("/users/dev/.config/cortex-harness"))
        .closed(posix("/users/dev/.local/state/cortex-harness"))
        .unwritable(posix("/users/dev/.local/share/cortex-harness"))
        .other_sessions(
            posix("/users/dev/.local/share/cortex-harness/sessions"),
            ProjectKey::new("aaaa1111"),
        );
    // Rules that allow everything, from every source that could.
    let set = set_with(
        vec![
            user(Effect::Read, "/**", Allow),
            user(Effect::Write, "/**", Allow),
            SourcedRule::new(rule(Effect::Read, "/**", Allow), RuleSource::Managed),
            SourcedRule::new(rule(Effect::Write, "~/**", Allow), RuleSource::Session),
        ],
        true,
        protected,
    );
    let read = |path: &str| action(set.path(&Effect::Read, &posix(path)));
    let write = |path: &str| action(set.path(&Effect::Write, &posix(path)));
    let sessions = "/users/dev/.local/share/cortex-harness/sessions";

    for closed in [
        "/users/dev/.config/cortex-harness",
        "/users/dev/.config/cortex-harness/credentials.toml",
        "/users/dev/.config/cortex-harness/config.toml",
        "/users/dev/.local/state/cortex-harness/trust.toml",
        "/users/dev/.local/state/cortex-harness/approvals/aaaa1111.toml",
    ] {
        assert_eq!(read(closed), Some(Deny), "read {closed}");
        assert_eq!(write(closed), Some(Deny), "write {closed}");
    }
    // Another project's sessions: neither read nor written.
    let other = format!("{sessions}/bbbb2222/s_1/log.jsonl");
    assert_eq!(read(&other), Some(Deny));
    assert_eq!(write(&other), Some(Deny));
    assert_eq!(read(sessions), Some(Deny), "the list of every project");
    // This project's own: read by rule, never written.
    let own = format!("{sessions}/aaaa1111/s_1/log.jsonl");
    assert_eq!(read(&own), Some(Allow));
    assert_eq!(write(&own), Some(Deny));
    assert_eq!(
        write("/users/dev/.local/share/cortex-harness/anything"),
        Some(Deny)
    );
    assert_eq!(
        read("/users/dev/.local/share/cortex-harness/anything"),
        Some(Allow)
    );
    // A name that only begins like a closed directory is another place.
    assert_eq!(
        read("/users/dev/.config/cortex-harness-notes/a"),
        Some(Allow)
    );
    // And the hit says it is built in.
    let hit = set
        .path(
            &Effect::Read,
            &posix("/users/dev/.config/cortex-harness/credentials.toml"),
        )
        .unwrap();
    assert!(matches!(hit.by, By::Builtin(_)));
    assert!(hit.describe().contains("credentials"));
}

// ---------------------------------------------------------------- hosts

#[test]
fn hosts_and_tools_are_matched_by_name() {
    let set = set(vec![
        user(Effect::Network, "docs.rs", Allow),
        user(Effect::Network, "*.example.com", Ask),
        user(Effect::Network, "Evil.Example.ORG", Deny),
        user(Effect::Ext, "docs/*", Allow),
        user(Effect::Ext, "docs/delete", Deny),
        user(Effect::Ext, "github/create_issue", Ask),
    ]);
    let host = |host: &str| action(set.network(Some(host)));
    assert_eq!(host("docs.rs"), Some(Allow));
    assert_eq!(host("DOCS.RS"), Some(Allow));
    assert_eq!(
        host("docs.rs."),
        Some(Allow),
        "a trailing dot is the same host"
    );
    assert_eq!(host("evildocs.rs"), None);
    assert_eq!(host("docs.rs.evil.net"), None);
    assert_eq!(host("api.example.com"), Some(Ask));
    assert_eq!(host("example.com"), None);
    assert_eq!(host("evil.example.org"), Some(Deny));
    // A call that cannot say where it goes meets every rule that holds
    // back, and no allow for one host.
    assert_eq!(action(set.network(None)), Some(Deny));
    let only_allows = super::RuleSet::compile(
        vec![user(Effect::Network, "docs.rs", Allow)],
        RuleContext {
            workspace: &posix("/work/app"),
            home: None,
            trusted: true,
        },
        Protected::none(),
    )
    .unwrap();
    assert_eq!(action(only_allows.network(None)), None);
    let any = set_with(
        vec![user(Effect::Network, "*", Allow)],
        true,
        Protected::none(),
    );
    assert_eq!(action(any.network(None)), Some(Allow));

    let tool = |tool: &str| action(set.ext(tool));
    assert_eq!(tool("docs/find"), Some(Allow));
    assert_eq!(tool("docs/delete"), Some(Deny));
    assert_eq!(tool("github/create_issue"), Some(Ask));
    assert_eq!(tool("github/list"), None);
    assert_eq!(tool("docs"), None);
    assert_eq!(tool("Docs/find"), None, "a tool's name is exact");
}

// ------------------------------------------------------------- commands

#[test]
fn a_command_rule_that_holds_back_matches_the_line_and_each_part() {
    let set = set(vec![
        user(Effect::Exec, "git push *", Deny),
        user(Effect::Exec, "rm *", Ask),
        user(Effect::Exec, "echo *b", Deny),
        user(Effect::Exec, "git *", Allow),
        user(Effect::Exec, "rm *", Allow),
    ]);
    let restriction = |line: &str| {
        let analysis = analyse(line, ShellSyntax::Posix);
        action(set.exec_restriction(line, analysis.parts(), ShellSyntax::Posix))
    };
    assert_eq!(restriction("git push origin main"), Some(Deny));
    assert_eq!(restriction("git status"), None);
    assert_eq!(restriction("cd repo && git push"), Some(Deny));
    assert_eq!(
        restriction("echo $(git push)"),
        Some(Deny),
        "a line that is not modelled"
    );
    assert_eq!(restriction("rm -rf target"), Some(Ask));
    assert_eq!(restriction("sudo rm -rf target"), Some(Ask));
    assert_eq!(
        restriction("rm x; git push"),
        Some(Deny),
        "the strongest of those that match"
    );
    // A quoted word with a space is two words to the loose match, and one
    // to the strict match of the part: the rule is tried both ways.
    assert_eq!(restriction("echo 'a b'"), Some(Deny));
    assert_eq!(restriction("echo 'a c'"), None);
}

// --------------------------------------------------------------- Windows

fn windows(path: &str) -> AbsPath {
    AbsPath::parse(path, PathStyle::Windows).unwrap()
}

fn windows_set(rules: Vec<SourcedRule>, protected: Protected) -> RuleSet {
    let workspace = windows(r"C:\Work\App");
    let home = windows(r"C:\People\Dev");
    RuleSet::compile(
        rules,
        RuleContext {
            workspace: &workspace,
            home: Some(&home),
            trusted: true,
        },
        protected,
    )
    .unwrap()
}

/// A deny or an ask is matched without regard to case, so no spelling gets
/// round it. An allow is matched in the stored case, so it never reaches a
/// folder that differs only in case, which NTFS allows in a directory
/// marked case-sensitive.
#[test]
fn on_windows_a_deny_folds_case_and_an_allow_never_over_grants() {
    let set = windows_set(
        vec![
            user(Effect::Write, r"**\.env*", Deny),
            user(Effect::Read, r"C:\People\Dev\Secret\**", Ask),
            user(Effect::Write, r"App\**", Allow),
            user(Effect::Read, r"~\Notes\**", Allow),
        ],
        Protected::none().closed(windows(r"C:\People\Dev\AppData\Roaming\cortex-harness")),
    );
    let read = |path: &str| action(set.path(&Effect::Read, &windows(path)));
    let write = |path: &str| action(set.path(&Effect::Write, &windows(path)));

    // Deny and ask: every casing.
    for spelling in [
        r"C:\Work\App\.env",
        r"C:\Work\App\.ENV",
        r"c:\work\app\.Env.Local",
    ] {
        assert_eq!(write(spelling), Some(Deny), "{spelling}");
    }
    for spelling in [
        r"C:\People\Dev\Secret\a",
        r"C:\PEOPLE\DEV\SECRET\a",
        r"c:\people\dev\secret\a",
    ] {
        assert_eq!(read(spelling), Some(Ask), "{spelling}");
    }
    // The built-in protection: every casing.
    for spelling in [
        r"C:\People\Dev\AppData\Roaming\cortex-harness\credentials.toml",
        r"C:\PEOPLE\DEV\APPDATA\ROAMING\CORTEX-HARNESS\CREDENTIALS.TOML",
    ] {
        assert_eq!(read(spelling), Some(Deny), "{spelling}");
        assert_eq!(write(spelling), Some(Deny), "{spelling}");
    }

    // An allow for `App` (inside the workspace `C:\Work\App`) grants `App`.
    assert_eq!(write(r"C:\Work\App\App\main.rs"), Some(Allow));
    // And not `app`, which may be another folder.
    assert_eq!(write(r"C:\Work\App\app\main.rs"), None);
    assert_eq!(write(r"C:\Work\App\APP\main.rs"), None);
    // Nor through another casing of the workspace's own names.
    assert_eq!(write(r"C:\work\app\App\main.rs"), None);
    // The drive letter has no case on disk.
    assert_eq!(write(r"c:\Work\App\App\main.rs"), Some(Allow));
    assert_eq!(read(r"C:\People\Dev\Notes\todo.md"), Some(Allow));
    assert_eq!(read(r"C:\People\Dev\notes\todo.md"), None);
    assert_eq!(read(r"C:\People\dev\Notes\todo.md"), None);
    // A deny still beats the allow, under any casing of either.
    assert_eq!(write(r"C:\Work\App\App\.ENV"), Some(Deny));
}

// ------------------------------------------------------------ properties

const PATTERNS: [&str; 12] = [
    "**",
    "src/**",
    "src/*.rs",
    "**/.env*",
    "*.lock",
    "docs/**/*.md",
    "/etc/**",
    "~/notes/**",
    "src/generated/**",
    "{src,tests}/**",
    "**/[!a-m]*",
    "secrets/",
];

const PATHS: [&str; 12] = [
    "/work/app/src/main.rs",
    "/work/app/src/generated/a.rs",
    "/work/app/src/.env",
    "/work/app/.env.local",
    "/work/app/Cargo.lock",
    "/work/app/docs/a/b.md",
    "/work/app/tests/it/main.rs",
    "/work/app/secrets/key",
    "/etc/passwd",
    "/users/dev/notes/todo.md",
    "/users/dev/.ssh/id_rsa",
    "/srv/other/src/main.rs",
];

// The properties below run a hundred or so cases each on a pull request, and
// fifty times that every night (`CORTEX_PROPERTY_SCALE`). Each case compiles
// rule sets, which is where the time goes in CI's unoptimised build: at
// three thousand cases each, this crate's properties took a third of the
// unit tier's budget on Windows.

fn any_rule(rng: &mut SeededRng) -> SourcedRule {
    let sources = [
        RuleSource::Managed,
        RuleSource::User,
        RuleSource::Project,
        RuleSource::Approved,
        RuleSource::Session,
    ];
    SourcedRule::new(
        rule(
            cases::pick(rng, &[Effect::Read, Effect::Write]).clone(),
            cases::pick(rng, &PATTERNS),
            *cases::pick(rng, &[Allow, Ask, Deny]),
        ),
        *cases::pick(rng, &sources),
    )
}

/// The property the build plan names: adding an `allow` never changes a
/// `deny`. And more: it never changes an `ask` either, and never makes any
/// answer stricter.
#[test]
fn adding_an_allow_never_changes_a_deny() {
    cases::check(
        "adding_an_allow_never_changes_a_deny",
        150,
        |rng| {
            let rules: Vec<SourcedRule> = (0..rng.below(6)).map(|_| any_rule(rng)).collect();
            let mut added = any_rule(rng);
            added.rule.action = Allow;
            (rules, added, rng.below(2) == 0)
        },
        |(rules, added, trusted)| {
            let before = set_with(rules.clone(), *trusted, Protected::none());
            let mut more = rules.clone();
            // Wherever the new rule is put.
            more.insert(more.len() / 2, added.clone());
            let after = set_with(more, *trusted, Protected::none());
            for effect in [Effect::Read, Effect::Write] {
                for path in PATHS {
                    let path = posix(path);
                    let (was, is) = (
                        action(before.path(&effect, &path)),
                        action(after.path(&effect, &path)),
                    );
                    match was {
                        Some(Deny) => ensure!(is == Some(Deny), "a deny became {is:?} on {path}"),
                        Some(Ask) => ensure!(is == Some(Ask), "an ask became {is:?} on {path}"),
                        Some(Allow) => ensure!(is == Some(Allow), "an allow became {is:?}"),
                        None => ensure!(
                            matches!(is, None | Some(Allow)),
                            "no rule became {is:?} on {path}"
                        ),
                    }
                }
            }
            Ok(())
        },
    );
}

/// The answer does not depend on the order the rules are in.
#[test]
fn the_order_of_the_rules_never_changes_an_answer() {
    cases::check(
        "rule_order_does_not_matter",
        120,
        |rng| {
            let rules: Vec<SourcedRule> = (0..rng.below(7)).map(|_| any_rule(rng)).collect();
            let mut shuffled = rules.clone();
            for index in (1..shuffled.len()).rev() {
                shuffled.swap(index, rng.below(index as u64 + 1) as usize);
            }
            (rules, shuffled)
        },
        |(rules, shuffled)| {
            let (first, second) = (set(rules.clone()), set(shuffled.clone()));
            for effect in [Effect::Read, Effect::Write] {
                for path in PATHS {
                    let path = posix(path);
                    ensure!(
                        action(first.path(&effect, &path)) == action(second.path(&effect, &path)),
                        "the order changed the answer on {path}"
                    );
                }
            }
            Ok(())
        },
    );
}

/// Trusting a project never takes a restriction away: whatever was denied
/// or asked about without trust still is with it. And without trust, a
/// project's rules never allow anything.
#[test]
fn a_projects_rules_never_loosen_before_trust() {
    cases::check(
        "project_rules_never_loosen_before_trust",
        150,
        |rng| {
            let users: Vec<SourcedRule> = (0..rng.below(4))
                .map(|_| {
                    let mut rule = any_rule(rng);
                    rule.source = RuleSource::User;
                    rule
                })
                .collect();
            let projects: Vec<SourcedRule> = (0..1 + rng.below(4))
                .map(|_| {
                    let mut rule = any_rule(rng);
                    rule.source = *cases::pick(rng, &[RuleSource::Project, RuleSource::Learned]);
                    rule
                })
                .collect();
            (users, projects)
        },
        |(users, projects)| {
            let alone = set_with(users.clone(), false, Protected::none());
            let mut both = users.clone();
            both.extend(projects.iter().cloned());
            let untrusted = set_with(both.clone(), false, Protected::none());
            let trusted = set_with(both, true, Protected::none());
            for effect in [Effect::Read, Effect::Write] {
                for path in PATHS {
                    let path = posix(path);
                    let without = action(alone.path(&effect, &path));
                    let with = action(untrusted.path(&effect, &path));
                    // Before trust the project can only tighten.
                    let tightened = match (without, with) {
                        (a, b) if a == b => true,
                        (None | Some(Allow), Some(Ask | Deny)) | (Some(Ask), Some(Deny)) => true,
                        _ => false,
                    };
                    ensure!(
                        tightened,
                        "an untrusted project changed {without:?} to {with:?} on {path}"
                    );
                    // And what it holds back, trust does not release.
                    if matches!(with, Some(Deny)) {
                        ensure!(
                            action(trusted.path(&effect, &path)) == Some(Deny),
                            "trust released a deny on {path}"
                        );
                    }
                }
            }
            Ok(())
        },
    );
}
