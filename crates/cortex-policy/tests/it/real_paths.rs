//! What a command names, followed on a real file system
//! (`docs/security.md`, section 7).
//!
//! The tests elsewhere read paths as text. These give the engine the real
//! resolver, so that a link in the workspace leads where the file system
//! says it leads, and on Windows a file is known under every spelling.

use std::path::Path;
use std::sync::Arc;

use cortex_base::{PathOptions, PathResolver};
use cortex_policy::command::ShellSyntax;
use cortex_policy::{
    Access, Engine, Locate, Outcome, Parts, Protected, Request, SandboxState, SourcedRule,
    default_sandbox,
};
use cortex_protocol::ids::LeaseId;
use cortex_protocol::lease::{Budget, HardLimits, Level, RuleAction};

use crate::clock;

/// An engine for the real workspace at `root`, with the real resolver.
fn real_engine(root: &Path, shell: ShellSyntax, rules: Vec<SourcedRule>) -> (Engine, PathResolver) {
    real_engine_at(root, shell, rules, Level::Supervised)
}

/// The same, at a level.
fn real_engine_at(
    root: &Path,
    shell: ShellSyntax,
    rules: Vec<SourcedRule>,
    level: Level,
) -> (Engine, PathResolver) {
    let resolver = PathResolver::new(root, PathOptions::new()).expect("open the workspace");
    let workspace = resolver.root().clone();
    let engine = Engine::start(Parts {
        lease_id: LeaseId::new("l_1"),
        level,
        rules,
        approved: Vec::new(),
        sandbox_policy: default_sandbox(&workspace, &[], &[]),
        sandbox: SandboxState::Enforcing,
        models: Vec::new(),
        budget: Budget::default(),
        limits: HardLimits::default(),
        workspace,
        home: None,
        trusted: true,
        repository_trusted: true,
        protected: Protected::none(),
        shell,
        locate: Arc::new(resolver.clone()),
        store: None,
        clock: clock(),
    })
    .unwrap();
    (engine, resolver)
}

fn run(engine: &Engine, resolver: &PathResolver, command: &str) -> Outcome {
    engine.decide(&Request {
        tool: "shell",
        default: RuleAction::Ask,
        accesses: &[
            Access::Exec {
                command: command.to_owned(),
                cwd: resolver.root().clone(),
            },
            Access::Write {
                path: resolver.root().clone(),
            },
        ],
    })
}

/// A pattern is expanded against the directory as it is now, and each match
/// is judged as the name written out. So `rm *` runs where nothing
/// protected, secret or denied is there, and a rule that holds a name back
/// is met through a pattern (`rm *.lock`). The owner's decision on the
/// second independent review of the fixes to step 12.
#[test]
fn a_pattern_is_judged_by_what_the_directory_holds() {
    use cortex_policy::RuleSource;
    use cortex_protocol::lease::{Effect, Rule};

    use crate::Said::{Allow, Ask};
    use crate::{reason, said};

    let scratch = cortex_testkit::scratch!("real_patterns");
    for file in [
        "ws/a.o",
        "ws/b.o",
        "ws/dist/app.js",
        "ws/dist/app.css",
        "ws/src/main.rs",
        "ws/Cargo.lock",
        "ws/keys/server.pem",
        "ws/.git/config",
    ] {
        scratch.file(file, "x");
    }
    for number in 0..1001 {
        scratch.file(format!("ws/big/{number}.txt"), "");
    }
    let ws = scratch.path().join("ws");
    let deny_lock = SourcedRule::new(
        Rule {
            effect: Effect::Write,
            pattern: "**/*.lock".to_owned(),
            action: RuleAction::Deny,
        },
        RuleSource::User,
    );
    let (plain, resolver) = real_engine_at(&ws, ShellSyntax::Posix, Vec::new(), Level::Autonomous);
    let (ruled, _) = real_engine_at(&ws, ShellSyntax::Posix, vec![deny_lock], Level::Autonomous);
    let decide = |engine: &Engine, command: &str| {
        let outcome = run(engine, &resolver, command);
        (said(&outcome), reason(&outcome))
    };
    // Nothing protected, secret or denied is there: it runs.
    for command in [
        "rm *",
        "rm *.o",
        "rm -rf dist/*",
        "rm src/*",
        "rm *.zip",
        "cat dist/*",
        "echo x > dist/*.js",
    ] {
        let (outcome, why) = decide(&plain, command);
        assert_eq!(outcome, Allow, "{command:?}: {why}");
    }
    // A rule that holds back a name is met through a pattern.
    for command in ["rm *.lock", "rm *", "rm Cargo.*"] {
        let (outcome, why) = decide(&ruled, command);
        assert_eq!(outcome, Ask, "{command:?}: {why}");
    }
    assert_eq!(decide(&ruled, "rm *.o").0, Allow);
    // A secret, a protected path, and more names than are listed.
    for command in [
        "rm keys/*",
        "cat keys/*",
        "rm -rf .g*",
        "cp evil .g*/config",
        "rm big/*",
        "rm big/*.txt",
    ] {
        let (outcome, why) = decide(&plain, command);
        assert_eq!(outcome, Ask, "{command:?}: {why}");
    }
    // After a part that may write, a listing taken now could miss what it
    // made: the pattern is judged by its text, and `*` could be `HEAD`.
    assert_eq!(decide(&plain, "touch new && rm *").0, Ask);
    assert_eq!(decide(&plain, "touch new | rm *").0, Ask);
    assert_eq!(decide(&plain, "touch new && rm *.o").0, Allow);
    // What the directory holds is judged after such a part too: its text
    // alone would let a rule be dodged (found by the final review of the
    // fixes to step 12).
    assert_eq!(decide(&ruled, "mkdir -p t && rm *.lock").0, Ask);
    assert_eq!(decide(&ruled, "mkdir -p t && rm *.o").0, Allow);
    // dash, and bash before 5.2, match `..` with `.?` and `.*`.
    for command in [
        "cd ./src && cp x .?/.git/hooks/pre-commit",
        "cd ./src && cp evil.yml .*/.github/workflows/ci.yml",
        "rm -rf .*",
    ] {
        let (outcome, why) = decide(&plain, command);
        assert_eq!(outcome, Ask, "{command:?}: {why}");
    }
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::symlink;

    use cortex_policy::RuleSource;
    use cortex_protocol::lease::{Effect, Rule};

    use super::*;
    use crate::Said::{Allow, Ask, ReadOnly};
    use crate::{reason, said};

    /// A link in the workspace can lead anywhere. What a command names is
    /// judged where it leads, not where it is written.
    #[test]
    fn a_link_out_of_the_workspace_is_judged_where_it_leads() {
        let scratch = cortex_testkit::scratch!("real_links");
        scratch.file("ws/src/main.rs", "fn main() {}");
        scratch.file("ws/.env", "TOKEN=1");
        scratch.file("outside/secret.txt", "secret");
        scratch.dir("ws/sub");
        let ws = scratch.join("ws");
        symlink("../outside/secret.txt", ws.join("link.txt")).unwrap();
        symlink("../outside", ws.join("dirlink")).unwrap();
        symlink(".env", ws.join("settings")).unwrap();
        symlink("src/main.rs", ws.join("inside-link")).unwrap();
        symlink("no-such-file", ws.join("dangling")).unwrap();
        symlink("/", ws.join("root")).unwrap();

        let allow_cp = SourcedRule::new(
            Rule {
                effect: Effect::Exec,
                pattern: "cp *".to_owned(),
                action: RuleAction::Allow,
            },
            RuleSource::User,
        );
        let (engine, resolver) = real_engine(&ws, ShellSyntax::Posix, vec![allow_cp]);
        for (command, expected) in [
            // Inside, by a plain name and through a link that stays inside.
            ("cat src/main.rs", ReadOnly),
            ("cat inside-link", ReadOnly),
            ("ls sub", ReadOnly),
            ("ls no-such-directory", ReadOnly),
            // Through a link that leaves.
            ("cat link.txt", Ask),
            ("cat ./link.txt", Ask),
            ("cat sub/../link.txt", Ask),
            ("cat dirlink/secret.txt", Ask),
            ("ls dirlink", Ask),
            ("ls -la dirlink/", Ask),
            ("find dirlink -type f", Ask),
            ("grep -r x dirlink", Ask),
            ("cd dirlink && ls", Ask),
            ("cd dirlink && cat secret.txt", Ask),
            ("cd ./dirlink && ls", Ask),
            ("cd ./dirlink && cat secret.txt", Ask),
            ("cd ./dirlink && echo x > new.txt", Ask),
            ("cat root/etc/passwd", Ask),
            ("wc -c src/main.rs link.txt", Ask),
            ("cat < link.txt", Ask),
            // Writing through one.
            ("echo x > dirlink/new.txt", Ask),
            ("echo x >> link.txt", Ask),
            ("cd dirlink && echo x > new.txt", Ask),
            // A link with an innocent name to a file that looks like a
            // secret.
            ("cat settings", Ask),
            ("head -1 settings", Ask),
            // A link to nothing cannot be checked.
            ("cat dangling", Ask),
            ("echo x > dangling", Ask),
        ] {
            let outcome = run(&engine, &resolver, command);
            assert_eq!(
                said(&outcome),
                expected,
                "{command:?}: {}",
                reason(&outcome)
            );
        }
        let why = reason(&run(&engine, &resolver, "cat link.txt"));
        assert!(why.contains("outside the workspace"), "{why}");
        assert!(
            why.contains("outside/secret.txt"),
            "the real location is named: {why}"
        );

        // `..` after a link is the parent of where the link leads.
        let outcome = run(&engine, &resolver, "cat dirlink/../ws/src/main.rs");
        assert_eq!(said(&outcome), ReadOnly, "{}", reason(&outcome));

        // A rule on the real file is met through the link, by a command a
        // rule allows as well as by one on the table.
        let outside = resolver.resolve("../outside").unwrap();
        let deny = SourcedRule::new(
            Rule {
                effect: Effect::Read,
                pattern: format!("{}/**", outside.abs().as_str()),
                action: RuleAction::Deny,
            },
            RuleSource::User,
        );
        let allow_cp = SourcedRule::new(
            Rule {
                effect: Effect::Exec,
                pattern: "cp *".to_owned(),
                action: RuleAction::Allow,
            },
            RuleSource::User,
        );
        let (engine, resolver) = real_engine(&ws, ShellSyntax::Posix, vec![deny, allow_cp]);
        assert_eq!(
            said(&run(&engine, &resolver, "cp src/main.rs copy.rs")),
            Allow
        );
        for command in [
            "cp link.txt copy.txt",
            "cp dirlink/secret.txt copy.txt",
            "cat link.txt",
        ] {
            let outcome = run(&engine, &resolver, command);
            assert_eq!(said(&outcome), Ask, "{command:?}");
            assert!(
                reason(&outcome).contains("deny read"),
                "{}",
                reason(&outcome)
            );
        }
    }

    /// Every POSIX shell's `cd` takes `..` logically: `a/../..` is two up
    /// from where the shell believes it is, whatever `a` links to. The file
    /// system takes it the other way, through the link. A workspace with a
    /// link `a` to `b/c` in it (a cloned repository can carry one) let
    /// `cd a/../.. && …` reach the workspace's parent while the engine
    /// judged the workspace (found by the review of step 12, with bash,
    /// dash and sh). A path a command names after the `cd` is opened from
    /// the real directory, so it is found from there.
    #[test]
    fn a_cd_through_a_link_takes_dot_dot_as_the_shell_does() {
        let scratch = cortex_testkit::scratch!("real_cd_logical");
        scratch.dir("home/ws/b/c");
        scratch.file("home/notes.txt", "outside");
        scratch.file("home/ws/notes.txt", "inside");
        let ws = scratch.path().join("home/ws");
        symlink("b/c", ws.join("a")).unwrap();
        let (engine, resolver) = real_engine(&ws, ShellSyntax::Posix, Vec::new());
        for (command, expected) in [
            ("cat ../notes.txt", Ask),
            ("cd .. && cat notes.txt", Ask),
            ("cd ./a/../.. && cat notes.txt", Ask),
            ("cd ./a/../.. && echo planted >> .bash_aliases", Ask),
            ("cd ./a/../.. && echo key >> .ssh/authorized_keys", Ask),
            ("cd a/../.. && cat notes.txt", Ask),
            ("cd ./a && cd .. && cd .. && cat notes.txt", Ask),
            // Logically back in the workspace.
            ("cd ./a/.. && cat notes.txt", ReadOnly),
            ("cd ./a && cd .. && cat notes.txt", ReadOnly),
            // From the real directory, `b/c`, two up is the workspace.
            ("cd ./a && cat ../../notes.txt", ReadOnly),
        ] {
            let outcome = run(&engine, &resolver, command);
            assert_eq!(
                said(&outcome),
                expected,
                "{command:?}: {}",
                reason(&outcome)
            );
        }
    }

    /// A pattern can match a link, and is followed through it as a name
    /// written out is. A link made earlier in the same line, which no
    /// listing taken now can see, makes every later part that names
    /// something one to ask about. The owner's decision on the second
    /// independent review of the fixes to step 12.
    #[test]
    fn a_pattern_is_followed_through_links_and_after_a_link_is_made() {
        let scratch = cortex_testkit::scratch!("real_pattern_links");
        scratch.file("ws/.github/workflows/ci.yml", "on: push");
        scratch.file("ws/.bashrc", "");
        scratch.file("ws/notes.txt", "");
        let ws = scratch.path().join("ws");
        symlink(".github", ws.join("ci")).unwrap();
        symlink(".bashrc", ws.join("a.o")).unwrap();
        let (engine, resolver) =
            super::real_engine_at(&ws, ShellSyntax::Posix, Vec::new(), Level::Autonomous);
        for command in [
            "cp evil.yml c?/workflows/x.yml",
            "cp evil.yml ci/workflows/x.yml",
            "cp x *.o",
            "echo x >> *.o",
            "ln -s .github gh && cp evil.yml gh/workflows/x.yml",
            "ln -s .github gh && cp evil.yml g?/workflows/x.yml",
            "ln -s .bashrc b.o; cp x *.o",
            "cp -s .github gh2 && cp x gh2/workflows/y.yml",
        ] {
            let outcome = run(&engine, &resolver, command);
            assert_eq!(said(&outcome), Ask, "{command:?}: {}", reason(&outcome));
        }
        assert_eq!(said(&run(&engine, &resolver, "rm *.txt")), Allow);
    }

    /// What the final review of the fixes to step 12 found on a real file
    /// system: `~\x` is a name outside Windows; bash takes a `cd` through
    /// links where the logical directory is not there; and a pattern after
    /// a part that writes still meets a link the directory holds.
    #[test]
    fn names_and_directories_resolve_as_the_shell_and_the_kernel_find_them() {
        let scratch = cortex_testkit::scratch!("real_final_review");
        scratch.file("ws/.github/workflows/ci.yml", "on: push");
        scratch.file("ws/.github/~\\x/k", "");
        scratch.file("ws/.github/x/k", "");
        let ws = scratch.path().join("ws");
        symlink(".github/x", ws.join("l")).unwrap();
        symlink(".github", ws.join("ci")).unwrap();
        let (supervised, resolver) = real_engine(&ws, ShellSyntax::Posix, Vec::new());
        // `~\x` is a name of its own outside Windows, found from where the
        // command runs.
        let github = resolver.root().join(".github").unwrap();
        assert_eq!(
            resolver.locate(&github, "~\\x/k"),
            Some(github.join("~\\x").unwrap().join("k").unwrap())
        );
        for command in [
            "cd ./.github && echo evil > '~\\x/../workflows/evil.yml'",
            "cd ./l/../workflows && echo evil > ci.yml",
        ] {
            let outcome = run(&supervised, &resolver, command);
            assert_eq!(said(&outcome), Ask, "{command:?}: {}", reason(&outcome));
        }
        let (autonomous, _) =
            super::real_engine_at(&ws, ShellSyntax::Posix, Vec::new(), Level::Autonomous);
        let outcome = run(
            &autonomous,
            &resolver,
            "mkdir -p t && cp evil.yml c?/workflows/ci.yml",
        );
        assert_eq!(said(&outcome), Ask, "{}", reason(&outcome));
    }

    /// The workspace itself opened through a link is known by its real
    /// location, so its own files are inside it.
    #[test]
    fn a_workspace_opened_through_a_link_is_its_real_location() {
        let scratch = cortex_testkit::scratch!("real_workspace_link");
        scratch.file("real/src/main.rs", "fn main() {}");
        symlink("real", scratch.join("alias")).unwrap();
        let (engine, resolver) =
            real_engine(&scratch.join("alias"), ShellSyntax::Posix, Vec::new());
        assert!(resolver.root().as_str().ends_with("/real"));
        assert_eq!(said(&run(&engine, &resolver, "cat src/main.rs")), ReadOnly);
        assert_eq!(said(&run(&engine, &resolver, "ls ..")), Ask);
    }
}

/// The Windows cases of the build plan: `cmd.exe` and PowerShell commands,
/// and a protected file reached under another spelling. These run on the
/// Windows CI job.
#[cfg(windows)]
mod windows {
    use cortex_policy::RuleSource;
    use cortex_protocol::lease::{Effect, Rule};

    use super::*;
    use crate::Said::{Allow, Ask, Deny, ReadOnly};
    use crate::{Said, in_ci, reason, said};

    fn user(effect: Effect, pattern: &str, action: RuleAction) -> SourcedRule {
        SourcedRule::new(
            Rule {
                effect,
                pattern: pattern.to_owned(),
                action,
            },
            RuleSource::User,
        )
    }

    #[test]
    fn powershell_commands_are_judged_on_the_real_file_system() {
        let scratch = cortex_testkit::scratch!("win_powershell");
        scratch.file("ws/Src/Main.rs", "fn main() {}");
        scratch.file("ws/README.md", "# hello");
        scratch.file("ws/.env", "TOKEN=1");
        scratch.file("ws/Private/notes.txt", "private");
        scratch.file("outside/Secret.txt", "secret");
        let ws = scratch.join("ws");
        let outside = scratch.join("outside").join("Secret.txt");
        let outside = outside.to_str().unwrap();

        let (engine, resolver) = real_engine(
            &ws,
            ShellSyntax::PowerShell,
            vec![
                user(Effect::Read, r"private\**", RuleAction::Deny),
                user(Effect::Exec, "cargo test *", RuleAction::Allow),
                user(Effect::Exec, "Remove-Item *", RuleAction::Deny),
            ],
        );
        let table: Vec<(String, Said)> = vec![
            // Reading the workspace, under any casing and either separator.
            ("Get-ChildItem".to_owned(), ReadOnly),
            ("Get-ChildItem -Recurse Src".to_owned(), ReadOnly),
            (r"Get-Content Src\Main.rs".to_owned(), ReadOnly),
            (r"Get-Content src\main.rs".to_owned(), ReadOnly),
            ("get-content SRC/MAIN.RS -TotalCount 3".to_owned(), ReadOnly),
            ("type README.md".to_owned(), ReadOnly),
            ("cat readme.MD".to_owned(), ReadOnly),
            (
                "Select-String -Pattern hello -Path README.md".to_owned(),
                ReadOnly,
            ),
            ("Test-Path Cargo.toml".to_owned(), ReadOnly),
            ("git status".to_owned(), ReadOnly),
            ("cargo test".to_owned(), Allow),
            ("CARGO.EXE test --workspace".to_owned(), Allow),
            ("cargo build".to_owned(), Ask),
            // A file that looks like a secret, under any casing.
            ("Get-Content .env".to_owned(), Ask),
            ("Get-Content .ENV".to_owned(), Ask),
            (r"Get-Content .\.Env".to_owned(), Ask),
            // What a rule denies, under any casing: a mention is asked
            // about.
            (r"Get-Content Private\notes.txt".to_owned(), Ask),
            (r"Get-Content PRIVATE\NOTES.TXT".to_owned(), Ask),
            ("Get-ChildItem private".to_owned(), Ask),
            // Outside the workspace.
            (format!("Get-Content {outside}"), Ask),
            (format!("Get-Content {}", outside.to_uppercase()), Ask),
            (r"Get-Content ..\outside\Secret.txt".to_owned(), Ask),
            ("Get-ChildItem ..".to_owned(), Ask),
            (r"Get-Content C:\Windows\win.ini".to_owned(), Ask),
            // Spellings the resolver refuses: none can be checked, so each
            // is asked about.
            ("Get-Content README.md.".to_owned(), Ask),
            ("Get-Content README.md:stream".to_owned(), Ask),
            ("Get-Content NUL".to_owned(), Ask),
            ("Get-Content CON".to_owned(), Ask),
            ("Get-Content COM1".to_owned(), Ask),
            (r"Get-Content \\.\C:\Windows\win.ini".to_owned(), Ask),
            (r"Get-Content \\?\C:\Windows\win.ini".to_owned(), Ask),
            (r"Get-Content \\server\share\file.txt".to_owned(), Ask),
            // A word git may read as a revision is a path when it starts
            // at a share, and one that cannot be followed.
            (
                r"git diff --no-index \\server\share\a README.md".to_owned(),
                Ask,
            ),
            ("git log -- //server/share/x".to_owned(), Ask),
            ("Get-Content C:README.md".to_owned(), Ask),
            (r"Get-Content \Windows\win.ini".to_owned(), Ask),
            ("Get-Content Env:PATH".to_owned(), Ask),
            (r"Get-ChildItem HKLM:\Software".to_owned(), Ask),
            // An alternate data stream written the way PowerShell expands.
            ("Get-Content README.md::$DATA".to_owned(), Ask),
            // More than one command, the call operator, an encoded command.
            ("Get-ChildItem; Get-Content .env".to_owned(), Ask),
            ("Get-Content README.md | Out-File copy.md".to_owned(), Ask),
            ("& cargo test".to_owned(), Ask),
            ("powershell -EncodedCommand ZQBjAGgAbwA=".to_owned(), Ask),
            ("cmd /c type README.md".to_owned(), Ask),
            // A deny, however the command is written.
            ("Remove-Item README.md".to_owned(), Deny),
            ("remove-item -Recurse Src".to_owned(), Deny),
            ("Get-ChildItem | Remove-Item".to_owned(), Deny),
            ("powershell -Command Remove-Item README.md".to_owned(), Deny),
        ];
        for (command, expected) in &table {
            let outcome = run(&engine, &resolver, command);
            assert_eq!(
                said(&outcome),
                *expected,
                "{command:?}: {}",
                reason(&outcome)
            );
        }
    }

    #[test]
    fn cmd_commands_are_never_analysed() {
        let scratch = cortex_testkit::scratch!("win_cmd");
        scratch.file("ws/README.md", "# hello");
        let ws = scratch.join("ws");
        let (engine, resolver) = real_engine(
            &ws,
            ShellSyntax::of_program(r"C:\Windows\System32\cmd.exe"),
            vec![
                user(Effect::Exec, "dir *", RuleAction::Allow),
                user(Effect::Exec, "type *", RuleAction::Allow),
                user(Effect::Exec, "*", RuleAction::Allow),
                user(Effect::Exec, "del *", RuleAction::Deny),
                user(Effect::Exec, "format *", RuleAction::Deny),
            ],
        );
        for (command, expected) in [
            ("dir", Ask),
            ("dir /s /b", Ask),
            ("type README.md", Ask),
            ("echo hello", Ask),
            ("git status", Ask),
            ("cd ..", Ask),
            ("dir & type README.md", Ask),
            ("echo %USERPROFILE%", Ask),
            ("type README.md > copy.md", Ask),
            ("for %f in (*) do echo %f", Ask),
            ("call build.bat", Ask),
            ("del README.md", Deny),
            ("DEL /Q /S *", Deny),
            ("dir & del README.md", Deny),
            ("dir && del README.md", Deny),
            ("dir | del README.md", Deny),
            ("(del README.md)", Deny),
            ("cmd /c \"del README.md\"", Deny),
            (r"C:\Windows\System32\cmd.exe /c del x", Deny),
            ("echo x&del y", Deny),
            ("format C:", Deny),
            ("FORMAT.COM C:", Deny),
        ] {
            let outcome = run(&engine, &resolver, command);
            assert_eq!(
                said(&outcome),
                expected,
                "{command:?}: {}",
                reason(&outcome)
            );
        }
        let why = reason(&run(&engine, &resolver, "dir"));
        assert!(why.contains("not analysed"), "{why}");
    }

    /// An allow rule written in any casing still reaches the folder it
    /// names, because the path it is matched against has been put in the
    /// file system's own spelling first. It does not reach a folder whose
    /// stored name differs.
    #[test]
    fn an_allow_matches_the_stored_spelling_of_what_was_asked_for() {
        let scratch = cortex_testkit::scratch!("win_allow_case");
        scratch.file("ws/README.md", "# hello");
        scratch.file("Notes/Todo.md", "todo");
        let ws = scratch.join("ws");
        // The folder as the file system spells it, whatever spelling the
        // test's own directory came in.
        let notes = PathResolver::new(&scratch.join("Notes"), PathOptions::new())
            .unwrap()
            .root()
            .to_native_string();
        let notes = std::path::PathBuf::from(notes);
        let pattern = format!(r"{}\**", notes.to_str().unwrap());
        let (engine, resolver) = real_engine(
            &ws,
            ShellSyntax::PowerShell,
            vec![user(Effect::Read, &pattern, RuleAction::Allow)],
        );
        let stored = notes.join("Todo.md");
        let stored = stored.to_str().unwrap();
        // However the command spells the path, it is the stored file.
        for spelling in [
            stored.to_owned(),
            stored.to_lowercase(),
            stored.to_uppercase(),
        ] {
            let outcome = run(&engine, &resolver, &format!("Get-Content {spelling}"));
            assert_eq!(said(&outcome), ReadOnly, "{spelling}: {}", reason(&outcome));
        }
        // The rule written in another casing than the folder is stored in
        // does not match: an allow is compared exactly, and under-grants.
        let lower = format!(r"{}\**", notes.to_str().unwrap().to_lowercase());
        let (engine, resolver) = real_engine(
            &ws,
            ShellSyntax::PowerShell,
            vec![user(Effect::Read, &lower, RuleAction::Allow)],
        );
        if lower != pattern {
            let outcome = run(&engine, &resolver, &format!("Get-Content {stored}"));
            assert_eq!(said(&outcome), Ask, "{}", reason(&outcome));
        }
    }

    /// In a directory marked case-sensitive, `App` and `app` are two
    /// folders. A session in `App` must not be able to write `app`
    /// unasked, and an allow for one must not grant the other.
    #[test]
    fn sys_a_folder_that_differs_only_in_case_is_outside_the_scope() {
        let scratch = cortex_testkit::scratch!("win_case_sensitive_scope");
        let parent = scratch.dir("sensitive");
        let made = std::process::Command::new("fsutil.exe")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(&parent)
            .arg("enable")
            .output();
        let made = matches!(&made, Ok(output) if output.status.success());
        if !made {
            assert!(
                !in_ci(),
                "the CI runner cannot make a case-sensitive directory, so this case did not run"
            );
            println!("skipped: cannot make a case-sensitive directory here");
            return;
        }
        std::fs::create_dir(parent.join("App")).unwrap();
        std::fs::create_dir(parent.join("app")).unwrap();
        std::fs::write(parent.join("App").join("mine.txt"), "upper").unwrap();
        std::fs::write(parent.join("app").join("theirs.txt"), "lower").unwrap();

        let allow = format!(r"{}\**", parent.join("App").to_str().unwrap());
        let (engine, resolver) = real_engine(
            &parent.join("App"),
            ShellSyntax::PowerShell,
            vec![user(Effect::Write, &allow, RuleAction::Allow)],
        );
        let decide = |access: Access| {
            said(&engine.decide(&Request {
                tool: "write",
                default: RuleAction::Allow,
                accesses: &[access],
            }))
        };
        let mine = resolver.resolve("mine.txt").unwrap();
        assert!(mine.is_inside_workspace());
        assert_eq!(
            decide(Access::Write {
                path: mine.abs().clone()
            }),
            Allow
        );

        let theirs = parent.join("app").join("theirs.txt");
        let theirs = resolver.resolve(theirs.to_str().unwrap()).unwrap();
        assert!(!theirs.is_inside_workspace());
        assert!(theirs.abs().as_str().ends_with("/sensitive/app/theirs.txt"));
        // Not in the scope, and not granted by the allow for `App`.
        assert_eq!(
            decide(Access::Write {
                path: theirs.abs().clone()
            }),
            Ask
        );
        assert_eq!(
            decide(Access::Read {
                path: theirs.abs().clone()
            }),
            Ask
        );
        let outcome = run(&engine, &resolver, r"Get-Content ..\app\theirs.txt");
        assert_eq!(said(&outcome), Ask, "{}", reason(&outcome));
        assert_eq!(
            said(&run(&engine, &resolver, "Get-Content mine.txt")),
            ReadOnly
        );
    }
}
