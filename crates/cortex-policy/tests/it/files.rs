//! Reads and writes by the file tools: the scope, files that look like
//! secrets, protected paths, the walk's gate and what a base state may
//! keep.

use std::sync::Arc;

use cortex_base::{AbsPath, PathStyle};
use cortex_policy::command::ShellSyntax;
use cortex_policy::{
    Access, Engine, Lexical, Parts, Protected, Request, SandboxState, default_sandbox,
};
use cortex_protocol::ids::LeaseId;
use cortex_protocol::lease::{Budget, Effect, HardLimits, Level, RuleAction};
use cortex_protocol::methods::ApprovalChoice;

use crate::Said::{Allow, Ask, Deny, ReadOnly};
use crate::{
    Said, at, clock, engine, engine_with, path, project, question, read, reason, said, user,
    with_rules, write,
};

#[test]
fn reads_and_edits_inside_the_workspace_run_and_outside_it_ask() {
    let engine = engine();
    let table: &[(&str, Said, Said)] = &[
        ("/work/app/src/main.rs", Allow, Allow),
        ("/work/app", Allow, Allow),
        ("/work/app/new/dir/file.txt", Allow, Allow),
        // The session's own temporary directory is in scope.
        ("/tmp/cortex-s1/scratch.txt", Allow, Allow),
        // Outside.
        ("/work/app2/src/main.rs", Ask, Ask),
        ("/work", Ask, Ask),
        ("/etc/hosts", Ask, Ask),
        ("/users/dev/notes.txt", Ask, Ask),
        ("/tmp/other", Ask, Ask),
        // Protected paths inside the workspace: read freely, written only
        // with an answer.
        ("/work/app/.git/hooks/pre-commit", Allow, Ask),
        ("/work/app/.git/config", Allow, Ask),
        ("/work/app/.cortex/config.toml", Allow, Ask),
        ("/work/app/.bashrc", Allow, Ask),
        // All of a repository's own data, in the workspace's repository
        // and in one nested in it.
        ("/work/app/.git/HEAD", Allow, Ask),
        ("/work/app/.git/objects/ab/cdef", Allow, Ask),
        ("/work/app/sub/.git/config", Allow, Ask),
        // What a CI service runs, and registry settings.
        ("/work/app/.github/workflows/ci.yml", Allow, Ask),
        ("/work/app/.cargo/config.toml", Allow, Ask),
        ("/work/app/.gitignore", Allow, Allow),
        // Files that look like secrets: asked about either way.
        ("/work/app/.env", Ask, Ask),
        ("/work/app/config/.env.production", Ask, Ask),
        ("/work/app/certs/server.pem", Ask, Ask),
        ("/work/app/deploy/id_ed25519", Ask, Ask),
        ("/work/app/.npmrc", Ask, Ask),
        // And files that only resemble them: not.
        ("/work/app/.env.example", Allow, Allow),
        ("/work/app/src/id_map.rs", Allow, Allow),
        ("/work/app/deploy/id_ed25519.pub", Allow, Allow),
        ("/work/app/src/env.rs", Allow, Allow),
        // The harness's own files: never.
        (
            "/users/dev/.config/cortex-harness/credentials.toml",
            Deny,
            Deny,
        ),
        (
            "/users/dev/.local/state/cortex-harness/trust.toml",
            Deny,
            Deny,
        ),
        (
            "/users/dev/.local/state/cortex-harness/approvals/aaaa1111.toml",
            Deny,
            Deny,
        ),
        (
            "/users/dev/.local/share/cortex-harness/sessions/bbbb2222/s_1/log.jsonl",
            Deny,
            Deny,
        ),
        // This project's own sessions: outside the workspace, so asked
        // about to read, and never written.
        (
            "/users/dev/.local/share/cortex-harness/sessions/aaaa1111/s_1/log.jsonl",
            Ask,
            Deny,
        ),
    ];
    for (file, reading, writing) in table {
        assert_eq!(said(&read(&engine, file)), *reading, "read {file}");
        assert_eq!(said(&write(&engine, file)), *writing, "write {file}");
    }
}

/// A file that looks like a secret is asked about by default. A user's
/// allow lifts that; a deny closes it; and a project's allow lifts it only
/// once the project is trusted.
#[test]
fn a_secret_looking_file_asks_by_default_and_a_rule_settles_it() {
    let env = "/work/app/.env";
    assert_eq!(said(&read(&engine(), env)), Ask);
    let why = reason(&read(&engine(), env));
    assert!(why.contains("holds secrets"), "{why}");

    let allowed = with_rules(vec![user(Effect::Read, "**/.env", RuleAction::Allow)]);
    assert_eq!(said(&read(&allowed, env)), Allow);
    assert_eq!(
        said(&write(&allowed, env)),
        Ask,
        "a read rule is not a write rule"
    );
    assert_eq!(said(&read(&allowed, "/work/app/.env.local")), Ask);

    let denied = with_rules(vec![
        user(Effect::Read, "**/.env*", RuleAction::Deny),
        user(Effect::Read, "**/.env", RuleAction::Allow),
    ]);
    assert_eq!(said(&read(&denied, env)), Deny);
    // A deny reaches a template too: it is a rule, not the default.
    assert_eq!(said(&read(&denied, "/work/app/.env.example")), Deny);

    let rules = || vec![project(Effect::Read, "**/.env", RuleAction::Allow)];
    let untrusted = engine_with(|parts| {
        parts.trusted = false;
        parts.rules = rules();
    });
    assert_eq!(
        said(&read(&untrusted, env)),
        Ask,
        "the project's allow waits for trust"
    );
    let trusted = with_rules(rules());
    assert_eq!(said(&read(&trusted, env)), Allow);

    // At `unrestricted` the holder decides, and a deny still holds.
    assert_eq!(said(&read(&at(Level::Unrestricted), env)), Allow);
    let unrestricted_denied = engine_with(|parts| {
        parts.level = Level::Unrestricted;
        parts.rules = vec![user(Effect::Read, "**/.env*", RuleAction::Deny)];
    });
    assert_eq!(said(&read(&unrestricted_denied, env)), Deny);
}

/// Projects can tighten and cannot loosen without trust, for paths.
#[test]
fn a_projects_path_rules_tighten_at_once_and_loosen_only_with_trust() {
    let rules = || {
        vec![
            project(Effect::Write, "~/**", RuleAction::Allow),
            project(Effect::Read, "/etc/**", RuleAction::Allow),
            project(Effect::Write, ".git/hooks/**", RuleAction::Allow),
            project(Effect::Write, "generated/**", RuleAction::Deny),
            project(Effect::Read, "fixtures/private/**", RuleAction::Ask),
        ]
    };
    let untrusted = engine_with(|parts| {
        parts.trusted = false;
        parts.rules = rules();
    });
    assert_eq!(said(&write(&untrusted, "/users/dev/.bashrc")), Ask);
    assert_eq!(said(&read(&untrusted, "/etc/passwd")), Ask);
    assert_eq!(
        said(&write(&untrusted, "/work/app/.git/hooks/pre-commit")),
        Ask
    );
    assert_eq!(said(&write(&untrusted, "/work/app/generated/a.rs")), Deny);
    assert_eq!(said(&read(&untrusted, "/work/app/fixtures/private/a")), Ask);
    assert_eq!(untrusted.withheld().len(), 3);

    let trusted = with_rules(rules());
    assert_eq!(said(&write(&trusted, "/users/dev/.bashrc")), Allow);
    assert_eq!(said(&read(&trusted, "/etc/passwd")), Allow);
    assert_eq!(
        said(&write(&trusted, "/work/app/.git/hooks/pre-commit")),
        Allow
    );
    assert_eq!(said(&write(&trusted, "/work/app/generated/a.rs")), Deny);
    assert_eq!(said(&read(&trusted, "/work/app/fixtures/private/a")), Ask);
    // Not even a trusted project's rule opens the harness's own files.
    assert_eq!(
        said(&write(
            &trusted,
            "/users/dev/.config/cortex-harness/config.toml"
        )),
        Deny
    );
}

#[test]
fn a_question_about_a_path_offers_that_one_path() {
    let engine = engine();
    let asked = question(read(&engine, "/etc/hosts"));
    assert_eq!(asked.lifts.len(), 1);
    assert_eq!(asked.lifts[0].effect, Effect::Read);
    assert_eq!(asked.lifts[0].pattern, "/etc/hosts");
    assert_eq!(asked.lifts[0].action, RuleAction::Allow);
    // With nowhere to keep it, "always allow in this project" is not on
    // offer (found by the review of step 12).
    assert_eq!(
        asked.choices(),
        [
            ApprovalChoice::AllowOnce,
            ApprovalChoice::AllowSession,
            ApprovalChoice::Deny
        ]
    );
    // A path whose name has pattern characters is still that one path.
    let odd = question(write(&engine, "/data/a*b/[x]/{y}.txt"));
    assert_eq!(odd.lifts[0].pattern, "/data/a[*]b/[[]x[]]/[{]y[}].txt");
    let lifted = with_rules(vec![cortex_policy::SourcedRule::new(
        odd.lifts[0].clone(),
        cortex_policy::RuleSource::Session,
    )]);
    assert_eq!(said(&write(&lifted, "/data/a*b/[x]/{y}.txt")), Allow);
    assert_eq!(said(&write(&lifted, "/data/aXb/x/y.txt")), Ask);
    assert_eq!(said(&write(&lifted, "/data/a*b/[x]/y.txt")), Ask);
    // A question an `ask` rule raised offers no rule: `ask` beats `allow`.
    let asking = with_rules(vec![user(Effect::Write, "migrations/**", RuleAction::Ask)]);
    let asked = question(write(&asking, "/work/app/migrations/1.sql"));
    assert!(asked.lifts.is_empty());
    assert_eq!(asked.choices().len(), 2);
    // What a front end that cannot ask tells the model.
    let refusal = asked.refusal();
    assert!(refusal.contains("nobody can be asked"), "{refusal}");
    assert!(refusal.contains("migrations/**"), "{refusal}");
}

#[test]
fn a_walk_is_decided_as_a_whole_and_then_entry_by_entry() {
    let engine = with_rules(vec![
        user(Effect::Read, "secrets/**", RuleAction::Deny),
        user(Effect::Read, "private/**", RuleAction::Ask),
        user(Effect::Read, "**/.env.shared", RuleAction::Allow),
    ]);
    let walk = |directory: &str| {
        said(&engine.decide(&Request {
            tool: "search",
            default: RuleAction::Allow,
            accesses: &[Access::ReadTree {
                path: path(directory),
            }],
        }))
    };
    assert_eq!(walk("/work/app"), Allow);
    assert_eq!(walk("/work/app/src"), Allow);
    assert_eq!(walk("/etc"), Ask);
    assert_eq!(walk("/work/app/secrets"), Deny, "a tree a rule closes");
    assert_eq!(walk("/work/app/private"), Ask);

    // The gate: what the walk may read of what it meets.
    for (entry, allowed) in [
        ("/work/app/src/main.rs", true),
        ("/work/app/src/id_map.rs", true),
        ("/work/app/.env.example", true),
        ("/work/app/.env.shared", true),
        ("/work/app/secrets/key.txt", false),
        ("/work/app/secrets", false),
        ("/work/app/private/notes.txt", false),
        ("/work/app/.env", false),
        ("/work/app/sub/.env.local", false),
        ("/work/app/certs/tls.key", false),
        ("/users/dev/.config/cortex-harness/credentials.toml", false),
    ] {
        assert_eq!(engine.may_read(&path(entry)), allowed, "{entry}");
    }
    // The gate does not ask about the scope again: a walk of a tree
    // outside the workspace, once approved, reads what is in it.
    assert!(engine.may_read(&path("/etc/hosts")));
    assert!(!engine.may_read(&path("/etc/ssl/private/server.key")));
    // At `unrestricted` the holder reads secrets; a deny still holds.
    let open = engine_with(|parts| {
        parts.level = Level::Unrestricted;
        parts.rules = vec![user(Effect::Read, "secrets/**", RuleAction::Deny)];
    });
    assert!(open.may_read(&path("/work/app/.env")));
    assert!(!open.may_read(&path("/work/app/secrets/key.txt")));
}

/// What a session's base state may keep: only what the agent could read
/// without asking, whatever the level.
#[test]
fn a_base_state_keeps_only_what_could_be_read_unasked() {
    let rules = || {
        vec![
            user(Effect::Read, "secrets/**", RuleAction::Deny),
            user(Effect::Read, "private/**", RuleAction::Ask),
            user(Effect::Read, "**/.env.shared", RuleAction::Allow),
        ]
    };
    for level in [Level::ReadOnly, Level::Supervised, Level::Unrestricted] {
        let engine = engine_with(|parts| {
            parts.level = level.clone();
            parts.rules = rules();
        });
        for (file, kept) in [
            ("/work/app/src/main.rs", true),
            // The earlier list hid this one: `id_*`.
            ("/work/app/src/id_map.rs", true),
            ("/work/app/.env.example", true),
            ("/work/app/.env.shared", true),
            ("/work/app/.env", false),
            ("/work/app/.env.local", false),
            ("/work/app/certs/server.pem", false),
            ("/work/app/deploy/id_rsa", false),
            ("/work/app/.netrc", false),
            ("/work/app/secrets/key.txt", false),
            ("/work/app/private/notes.txt", false),
            // Outside the workspace: nothing a base state should hold.
            ("/etc/hosts", false),
            ("/users/dev/.config/cortex-harness/credentials.toml", false),
        ] {
            assert_eq!(engine.may_store(&path(file)), kept, "{file} at {level:?}");
        }
    }
}

// ------------------------------------------------------------ on Windows

fn windows(text: &str) -> AbsPath {
    AbsPath::parse(text, PathStyle::Windows).unwrap()
}

/// An engine whose paths are Windows paths, read as text, so that the
/// rules about case are tested on every OS.
fn windows_engine(rules: Vec<cortex_policy::SourcedRule>) -> Engine {
    Engine::start(windows_parts(rules)).unwrap()
}

fn windows_parts(rules: Vec<cortex_policy::SourcedRule>) -> Parts {
    let workspace = windows(r"C:\Work\App");
    let home = windows(r"C:\People\Dev");
    Parts {
        lease_id: LeaseId::new("l_1"),
        level: Level::Supervised,
        rules,
        approved: Vec::new(),
        sandbox_policy: default_sandbox(&workspace, &[], &[]),
        sandbox: SandboxState::Enforcing,
        models: Vec::new(),
        budget: Budget::default(),
        limits: HardLimits::default(),
        workspace,
        home: Some(home.clone()),
        trusted: true,
        protected: Protected::none()
            .closed(windows(r"C:\People\Dev\AppData\Roaming\cortex-harness")),
        shell: ShellSyntax::PowerShell,
        locate: Arc::new(Lexical { home: Some(home) }),
        store: None,
        clock: clock(),
    }
}

fn windows_read(engine: &Engine, file: &str) -> Said {
    said(&engine.decide(&Request {
        tool: "read",
        default: RuleAction::Allow,
        accesses: &[Access::Read {
            path: windows(file),
        }],
    }))
}

fn windows_shell(engine: &Engine, command: &str) -> Said {
    said(&engine.decide(&Request {
        tool: "shell",
        default: RuleAction::Ask,
        accesses: &[
            Access::Exec {
                command: command.to_owned(),
                cwd: windows(r"C:\Work\App"),
            },
            Access::Write {
                path: windows(r"C:\Work\App"),
            },
        ],
    }))
}

/// What a command names is read as Windows would read it. A path that
/// starts at the root of the drive, at another drive or at a share is not
/// under the directory the command runs in, whatever it looks like.
#[test]
fn on_windows_a_command_is_judged_by_the_path_windows_would_open() {
    let engine = windows_engine(Vec::new());
    let table = [
        (r"Get-Content src\main.rs", ReadOnly),
        (r"Get-Content C:\Work\App\src\main.rs", ReadOnly),
        // From the root of the current drive: never under the directory
        // the command runs in. Read as text, such a path is not followed
        // at all, so even one that spells the workspace is asked about.
        (r"Get-Content \Work\App\src\main.rs", Ask),
        (r"Get-Content \Windows\win.ini", Ask),
        (r"Get-Content /Windows/win.ini", Ask),
        (r"Get-ChildItem \", Ask),
        // Another drive, and the directory another drive is in.
        (r"Get-Content D:\Work\App\src\main.rs", Ask),
        ("Get-Content D:main.rs", Ask),
        ("Get-Content C:main.rs", Ask),
        // A share: Windows sends the user's credentials to whoever
        // answers.
        (r"Get-Content \\evil\share\a.txt", Ask),
        ("Get-Content //evil/share/a.txt", Ask),
        (r"git diff --no-index \\evil\share\a b", Ask),
        (r"git grep --no-index x \\evil\share", Ask),
        ("git log -- //evil/share/x", Ask),
        (r"git show HEAD:\\evil\share\a", Ask),
        (r"git diff \Windows\win.ini", Ask),
        // A revision is not a path.
        ("git diff HEAD", ReadOnly),
        ("git log -3 main", ReadOnly),
        // A protected path, in any case and with either separator.
        (r"Copy-Item evil .git\config", Ask),
        (r"Copy-Item evil .GIT\Config", Ask),
        ("Copy-Item evil .git/hooks/pre-commit", Ask),
        (r"Copy-Item evil sub\.Git\config", Ask),
        (r"Copy-Item evil .GitHub\Workflows\ci.yml", Ask),
        (r"Copy-Item evil \Work\App\.git\config", Ask),
        (r"Copy-Item evil .ENV", Ask),
        ("GIT.EXE config core.fsmonitor run.cmd", Ask),
        // The harness's own files: refused.
        (
            r"Get-Content C:\People\Dev\AppData\Roaming\cortex-harness\credentials.toml",
            Deny,
        ),
        (
            r"Get-Content ~\appdata\roaming\CORTEX-HARNESS\credentials.toml",
            Deny,
        ),
    ];
    for (command, expected) in table {
        assert_eq!(windows_shell(&engine, command), expected, "{command:?}");
    }
    // Where commands run unasked, a protected path named in any case is
    // still asked about.
    let autonomous = {
        let mut parts = windows_parts(Vec::new());
        parts.level = Level::Autonomous;
        Engine::start(parts).unwrap()
    };
    for command in [
        r"Copy-Item evil .GIT\Config",
        r"Copy-Item evil sub\.git\CONFIG",
        r"Copy-Item evil .Cargo\Config.toml",
        "git.exe config core.fsmonitor run.cmd",
    ] {
        assert_eq!(windows_shell(&autonomous, command), Ask, "{command:?}");
    }
    assert_eq!(
        windows_shell(&autonomous, r"Copy-Item a.txt src\b.txt"),
        Allow
    );
}

fn windows_write(engine: &Engine, file: &str) -> Said {
    said(&engine.decide(&Request {
        tool: "write",
        default: RuleAction::Allow,
        accesses: &[Access::Write {
            path: windows(file),
        }],
    }))
}

/// On Windows what grants compares names exactly, and what holds back
/// compares them without regard to case.
#[test]
fn on_windows_the_scope_and_an_allow_are_exact_and_a_deny_folds() {
    let engine = windows_engine(vec![
        user(Effect::Write, r"**\generated\**", RuleAction::Deny),
        user(Effect::Read, r"C:\People\Dev\Notes\**", RuleAction::Allow),
    ]);
    // The scope: `C:\Work\App` as it is stored, and under any drive-letter
    // case.
    assert_eq!(windows_write(&engine, r"C:\Work\App\src\main.rs"), Allow);
    assert_eq!(windows_write(&engine, r"c:\Work\App\src\main.rs"), Allow);
    // `app` may be another folder than `App`, in a directory marked
    // case-sensitive: it is not in the scope.
    assert_eq!(windows_write(&engine, r"C:\Work\app\src\main.rs"), Ask);
    assert_eq!(windows_write(&engine, r"C:\work\App\src\main.rs"), Ask);
    assert_eq!(windows_read(&engine, r"C:\Work\APP\src\main.rs"), Ask);
    // An allow: only the spelling it names.
    assert_eq!(windows_read(&engine, r"C:\People\Dev\Notes\todo.md"), Allow);
    assert_eq!(windows_read(&engine, r"C:\People\Dev\notes\todo.md"), Ask);
    // A deny, a protected path and the built-in protection: every casing.
    for spelling in [r"C:\Work\App\generated\a.rs", r"C:\Work\App\GENERATED\a.rs"] {
        assert_eq!(windows_write(&engine, spelling), Deny, "{spelling}");
    }
    for spelling in [
        r"C:\Work\App\.git\hooks\pre-commit",
        r"C:\Work\App\.GIT\Hooks\pre-commit",
    ] {
        assert_eq!(windows_write(&engine, spelling), Ask, "{spelling}");
    }
    for spelling in [
        r"C:\People\Dev\AppData\Roaming\cortex-harness\credentials.toml",
        r"c:\people\dev\appdata\roaming\CORTEX-HARNESS\credentials.toml",
    ] {
        assert_eq!(windows_read(&engine, spelling), Deny, "{spelling}");
    }
    // A file that looks like a secret, in any case.
    assert_eq!(windows_read(&engine, r"C:\Work\App\.ENV"), Ask);
    assert_eq!(windows_read(&engine, r"C:\Work\App\Server.PEM"), Ask);
}
