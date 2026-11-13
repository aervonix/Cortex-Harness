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
    Access, Engine, Outcome, Parts, Protected, Request, SandboxState, SourcedRule, default_sandbox,
};
use cortex_protocol::ids::LeaseId;
use cortex_protocol::lease::{Budget, HardLimits, Level, RuleAction};

use crate::{Scratch, clock};

/// An engine for the real workspace at `root`, with the real resolver.
fn real_engine(root: &Path, shell: ShellSyntax, rules: Vec<SourcedRule>) -> (Engine, PathResolver) {
    let resolver = PathResolver::new(root, PathOptions::new()).expect("open the workspace");
    let workspace = resolver.root().clone();
    let engine = Engine::start(Parts {
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
        home: None,
        trusted: true,
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
        let scratch = Scratch::new("real_links");
        scratch.file("ws/src/main.rs", "fn main() {}");
        scratch.file("ws/.env", "TOKEN=1");
        scratch.file("outside/secret.txt", "secret");
        scratch.dir("ws/sub");
        let ws = scratch.dir.join("ws");
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
            assert_eq!(said(&outcome), expected, "{command:?}: {}", reason(&outcome));
        }
        let why = reason(&run(&engine, &resolver, "cat link.txt"));
        assert!(why.contains("outside the workspace"), "{why}");
        assert!(why.contains("outside/secret.txt"), "the real location is named: {why}");

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
        assert_eq!(said(&run(&engine, &resolver, "cp src/main.rs copy.rs")), Allow);
        for command in ["cp link.txt copy.txt", "cp dirlink/secret.txt copy.txt", "cat link.txt"] {
            let outcome = run(&engine, &resolver, command);
            assert_eq!(said(&outcome), Ask, "{command:?}");
            assert!(reason(&outcome).contains("deny read"), "{}", reason(&outcome));
        }
    }

    /// The workspace itself opened through a link is known by its real
    /// location, so its own files are inside it.
    #[test]
    fn a_workspace_opened_through_a_link_is_its_real_location() {
        let scratch = Scratch::new("real_workspace_link");
        scratch.file("real/src/main.rs", "fn main() {}");
        symlink("real", scratch.dir.join("alias")).unwrap();
        let (engine, resolver) =
            real_engine(&scratch.dir.join("alias"), ShellSyntax::Posix, Vec::new());
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
        let scratch = Scratch::new("win_powershell");
        scratch.file("ws/Src/Main.rs", "fn main() {}");
        scratch.file("ws/README.md", "# hello");
        scratch.file("ws/.env", "TOKEN=1");
        scratch.file("ws/Private/notes.txt", "private");
        scratch.file("outside/Secret.txt", "secret");
        let ws = scratch.dir.join("ws");
        let outside = scratch.dir.join("outside").join("Secret.txt");
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
            ("Select-String -Pattern hello -Path README.md".to_owned(), ReadOnly),
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
            assert_eq!(said(&outcome), *expected, "{command:?}: {}", reason(&outcome));
        }
    }

    #[test]
    fn cmd_commands_are_never_analysed() {
        let scratch = Scratch::new("win_cmd");
        scratch.file("ws/README.md", "# hello");
        let ws = scratch.dir.join("ws");
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
            assert_eq!(said(&outcome), expected, "{command:?}: {}", reason(&outcome));
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
        let scratch = Scratch::new("win_allow_case");
        scratch.file("ws/README.md", "# hello");
        scratch.file("Notes/Todo.md", "todo");
        let ws = scratch.dir.join("ws");
        // The folder as the file system spells it, whatever spelling the
        // test's own directory came in.
        let notes = PathResolver::new(&scratch.dir.join("Notes"), PathOptions::new())
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
        for spelling in [stored.to_owned(), stored.to_lowercase(), stored.to_uppercase()] {
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
        let scratch = Scratch::new("win_case_sensitive_scope");
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
        assert_eq!(decide(Access::Write { path: mine.abs().clone() }), Allow);

        let theirs = parent.join("app").join("theirs.txt");
        let theirs = resolver.resolve(theirs.to_str().unwrap()).unwrap();
        assert!(!theirs.is_inside_workspace());
        assert!(theirs.abs().as_str().ends_with("/sensitive/app/theirs.txt"));
        // Not in the scope, and not granted by the allow for `App`.
        assert_eq!(decide(Access::Write { path: theirs.abs().clone() }), Ask);
        assert_eq!(decide(Access::Read { path: theirs.abs().clone() }), Ask);
        let outcome = run(&engine, &resolver, r"Get-Content ..\app\theirs.txt");
        assert_eq!(said(&outcome), Ask, "{}", reason(&outcome));
        assert_eq!(said(&run(&engine, &resolver, "Get-Content mine.txt")), ReadOnly);
    }
}
