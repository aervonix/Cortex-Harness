//! The table of commands with expected outcomes
//! (`docs/build-plan.md`, step 12): what the engine says of a `shell` call,
//! at the default level, in a trusted project, with no rules and then with
//! some.

use cortex_policy::command::ShellSyntax;
use cortex_protocol::lease::{Effect, Level, RuleAction};

use crate::Said::{Allow, Ask, Deny, ReadOnly};
use crate::{
    Said, at, engine, engine_with, project, question, reason, said, shell, shell_in, user,
    with_rules,
};

/// With no rules, at `supervised`: a command that only reads runs and is
/// marked read-only, and every other command is asked about.
#[test]
fn the_command_table_at_the_default_level() {
    let table: &[(&str, Said)] = &[
        // Reading the workspace.
        ("ls", ReadOnly),
        ("ls -la src", ReadOnly),
        ("cat README.md", ReadOnly),
        ("cat src/main.rs | head -20", ReadOnly),
        (
            "grep -n TODO src/main.rs | sort | uniq -c | sort -rn | head -5",
            ReadOnly,
        ),
        ("rg -n 'fn main' crates", ReadOnly),
        ("rg TODO", ReadOnly),
        ("git log --oneline -5 && git diff", ReadOnly),
        ("find . -name '*.rs' -type f", ReadOnly),
        ("wc -l src/main.rs && pwd", ReadOnly),
        ("git status", ReadOnly),
        ("git status --short; git log --oneline -5", ReadOnly),
        ("git diff --stat HEAD~1", ReadOnly),
        ("git branch --show-current || true", ReadOnly),
        ("sed -n '10,20p' src/main.rs", ReadOnly),
        ("echo hello", ReadOnly),
        ("date", ReadOnly),
        ("ls 2>/dev/null", ReadOnly),
        ("ls > /dev/null 2>&1", ReadOnly),
        ("cd src && ls", ReadOnly),
        ("cd src && cat main.rs && cd .. && git status", ReadOnly),
        ("cd src", ReadOnly),
        ("ls\npwd\n", ReadOnly),
        ("cat < Cargo.toml", ReadOnly),
        ("cat < /dev/null", ReadOnly),
        ("ls; cd src && cat main.rs", ReadOnly),
        ("ls && cd src && cat main.rs | head -3", ReadOnly),
        ("ls || cd src", ReadOnly),
        // Reading, and writing inside the workspace by a redirection: the
        // command is safe, the edit is in scope, and it is not read-only.
        ("echo hello > notes.txt", Allow),
        ("ls -la >> listing.txt", Allow),
        ("git status > status.txt && cat status.txt", Allow),
        // Commands that are on no safe list.
        ("cargo build", Ask),
        ("cargo test --workspace", Ask),
        ("npm install", Ask),
        ("make", Ask),
        ("rm -rf target", Ask),
        ("mkdir build", Ask),
        ("touch x", Ask),
        ("mv a b", Ask),
        ("cp a b", Ask),
        ("chmod +x run.sh", Ask),
        ("./run.sh", Ask),
        ("python3 script.py", Ask),
        ("curl https://example.com", Ask),
        ("git push", Ask),
        ("git commit -m msg", Ask),
        ("git add .", Ask),
        ("git checkout main", Ask),
        ("git stash", Ask),
        ("env", Ask),
        ("tee out.txt", Ask),
        ("less README.md", Ask),
        ("diff a b", Ask),
        // One part off the list takes the line off it.
        ("ls && cargo build", Ask),
        ("cargo build && ls", Ask),
        ("ls | xargs rm", Ask),
        ("cat a.txt | sh", Ask),
        ("git status; rm -rf x", Ask),
        ("ls || make", Ask),
        // Options that write or run, on commands that otherwise only read.
        ("sort -o out.txt data.txt", Ask),
        ("find . -delete", Ask),
        ("find . -name x -exec rm x ;", Ask),
        ("git diff --output=x.diff", Ask),
        ("git branch new-branch", Ask),
        ("git -c core.pager=evil log", Ask),
        ("sed -i s/a/b/ f", Ask),
        ("rg --pre cat x", Ask),
        ("tail -f log.txt", Ask),
        // A search of a whole tree that reads hidden files, where the
        // files that look like secrets are.
        ("grep -rn TODO src", Ask),
        ("grep -r password .", Ask),
        ("rg --hidden TODO", Ask),
        ("rg -uu TODO", Ask),
        ("rg --no-ignore TODO", Ask),
        ("rg -g .env KEY", Ask),
        // Shell builtins that evaluate their arguments.
        ("printf '%s' hello", Ask),
        ("test -f Cargo.toml", Ask),
        ("printf -v 'a[$(touch pwned)]' %s 1", Ask),
        ("test -v 'a[$(touch pwned)]'", Ask),
        ("printf -v CDPATH %s ~ && cd .aws && cat credentials", Ask),
        // A command named by a path, not by its name.
        ("./ls", Ask),
        ("/tmp/ls", Ask),
        ("bin/cat README.md", Ask),
        // Globs: the names they expand to are not known.
        ("ls *.rs", Ask),
        ("cat src/*.rs", Ask),
        ("wc -l *", Ask),
        // What a command names is a read like any other.
        ("cat /etc/passwd", Ask),
        ("cat ~/.ssh/id_rsa", Ask),
        ("ls ~", Ask),
        ("ls ..", Ask),
        ("cat ../other/secret.txt", Ask),
        ("grep -r password /etc", Ask),
        ("head -5 .env", Ask),
        ("cat config/.env.production", Ask),
        ("cat certs/server.pem", Ask),
        ("git show HEAD:.env", Ask),
        ("find / -name id_rsa", Ask),
        ("find . -newer /etc/passwd", Ask),
        ("wc -l src/main.rs /etc/hosts", Ask),
        ("cat < /etc/passwd", Ask),
        ("cat < .env", Ask),
        ("cd /etc && cat passwd", Ask),
        ("cd /etc && ls", Ask),
        ("cd .. && ls", Ask),
        ("cd ~ && ls", Ask),
        ("realpath --relative-to=/etc x", Ask),
        // The harness's own files: refused, however they are named.
        ("cat ~/.config/cortex-harness/credentials.toml", Deny),
        ("cat ~/.local/state/cortex-harness/trust.toml", Deny),
        ("ls ~/.local/share/cortex-harness/sessions/bbbb2222", Deny),
        ("cd ~/.config/cortex-harness && ls", Deny),
        // A tree with the harness's files somewhere in it.
        ("rg token /users/dev", Ask),
        // Redirections that write outside the workspace, or to a secret or
        // a protected path.
        ("echo x > /tmp/elsewhere.txt", Ask),
        ("echo x > ../outside.txt", Ask),
        ("echo x > ~/.bashrc", Ask),
        ("echo x > .env", Ask),
        ("echo x >> .git/hooks/pre-commit", Ask),
        ("echo x > .git/config", Ask),
        ("echo x > .cortex/config.toml", Ask),
        ("ls > *.txt", Ask),
        // A `cd` whose effect cannot be known.
        ("cd src; ls", Ask),
        ("cd src || ls", Ask),
        ("ls | cd src && cat x", Ask),
        ("cd && ls", Ask),
        ("cd - && ls", Ask),
        ("cd a b && ls", Ask),
        ("cd src > log && ls", Ask),
        // A `cd` that may not run, or after which a part runs whether it
        // succeeded or not. Each of these reads `/work/notes.txt`, outside
        // the workspace, if the `cd` is taken to have happened when it has
        // not, or the other way round.
        ("true || cd src && cat ../notes.txt", Ask),
        ("false || cd src && cat ../notes.txt", Ask),
        ("ls || cd src && cat main.rs", Ask),
        ("cd nowhere && ls; cat ../notes.txt", Ask),
        ("cd src && ls; cat ../notes.txt", Ask),
        ("cd src && false || cat ../notes.txt", Ask),
        ("cd src && ls || cat ../notes.txt", Ask),
        ("cd src && ls\ncat ../notes.txt", Ask),
        ("true || cd src && echo key >> ../authorized_keys", Ask),
        // A `cd` with a redirection writes the file, whatever else it does.
        ("cd src > log", Ask),
        ("cd . > notes.txt", Ask),
        ("cd . > ~/.bashrc", Ask),
        ("cd . > ../outside.txt", Ask),
        ("cd . > .git/hooks/pre-commit", Ask),
    ];
    let engine = engine();
    for (command, expected) in table {
        let outcome = shell(&engine, command);
        assert_eq!(
            said(&outcome),
            *expected,
            "{command:?}: {}",
            reason(&outcome)
        );
    }
}

/// What rules add: an allow for a command, a deny that no allow lifts, an
/// ask, and that an allow needs every part.
#[test]
fn the_command_table_with_rules() {
    let engine = with_rules(vec![
        user(Effect::Exec, "cargo test *", RuleAction::Allow),
        user(Effect::Exec, "cargo build *", RuleAction::Allow),
        user(Effect::Exec, "make", RuleAction::Allow),
        user(Effect::Exec, "git *", RuleAction::Allow),
        user(Effect::Exec, "cat *", RuleAction::Allow),
        user(Effect::Exec, "git push *", RuleAction::Ask),
        user(Effect::Exec, "git push --force*", RuleAction::Deny),
        user(Effect::Exec, "rm *", RuleAction::Deny),
        user(Effect::Write, "**/.env*", RuleAction::Deny),
        user(Effect::Read, "secrets/**", RuleAction::Deny),
        user(Effect::Read, "private/**", RuleAction::Ask),
    ]);
    let table: &[(&str, Said)] = &[
        ("cargo test", Allow),
        ("cargo test --workspace -- --nocapture", Allow),
        ("cargo build --release && cargo test", Allow),
        ("make", Allow),
        ("make install", Ask),
        ("cargo run", Ask),
        ("cargo test && cargo run", Ask),
        ("cargo test | tee log.txt", Ask),
        // An allowed command and a read-only one: allowed, not read-only.
        ("cargo test && git status", Allow),
        ("git status", ReadOnly),
        ("git commit -m msg", Allow),
        ("git stash", Allow),
        // `ask` beats `allow`, and `deny` beats both.
        ("git push", Ask),
        ("git push origin main", Ask),
        ("git push --force", Deny),
        ("git push --force-with-lease origin main", Deny),
        ("cd repo && git push --force", Deny),
        ("rm -rf target", Deny),
        ("cargo test && rm -rf target", Deny),
        ("sudo rm -rf /", Deny),
        ("/bin/rm x", Deny),
        ("git status; rm x", Deny),
        // A deny matches a line the analysis does not model, too.
        ("echo $(rm x)", Deny),
        ("x=1 rm y", Deny),
        ("bash -c 'rm -rf x'", Deny),
        ("for f in *; do rm $f; done", Deny),
        // No allow rule applies to a line that is not modelled.
        ("cargo test $EXTRA", Ask),
        ("cargo test `id`", Ask),
        ("cargo test; (cargo run)", Ask),
        ("FOO=1 cargo test", Ask),
        ("cargo test &", Ask),
        ("cargo test <<EOF\nx\nEOF", Ask),
        ("eval cargo test", Ask),
        ("cargo test # then more", Ask),
        ("cargo test\\\n --all", Ask),
        // A path a rule protects, named by a command that is allowed.
        ("cat secrets/key.txt", Ask),
        ("cat private/notes.txt", Ask),
        ("git add secrets/key.txt", Ask),
        ("cargo test --manifest-path=secrets/Cargo.toml", Ask),
        ("cat ~/.config/cortex-harness/credentials.toml", Deny),
        ("git add ~/.config/cortex-harness/credentials.toml", Deny),
        ("git add src/.env", Ask),
        // A search reads files nobody named: asked about where a rule
        // protects something in the tree it searches.
        ("rg KEY", Ask),
        ("rg KEY .", Ask),
        ("rg -e KEY", Ask),
        ("rg --files", Ask),
        ("rg KEY secrets", Ask),
        ("rg KEY private/deep", Ask),
        ("rg KEY src", ReadOnly),
        ("rg KEY src tests", ReadOnly),
        ("rg KEY src .", Ask),
        ("cd src && rg KEY", ReadOnly),
        ("cd src && rg KEY ..", Ask),
        ("git grep KEY", Ask),
        ("git diff", Ask),
        ("git show HEAD", Ask),
        ("git log -p", Ask),
        ("cd src && git log -p", Ask),
        ("git log --oneline -5", ReadOnly),
        ("git status --short", ReadOnly),
        // Commands that print names, and not what files hold.
        ("ls -R", ReadOnly),
        ("find . -name '*.txt'", ReadOnly),
        // A redirection to a file a write rule denies is denied.
        ("echo x > src/.env", Deny),
        ("cargo test > .env.log", Deny),
        ("cargo test > test.log", Allow),
        // An allow for `cat` still leaves a secret asked about, because the
        // table's check of what it names comes first.
        ("cat .env", Ask),
        ("cat README.md", ReadOnly),
    ];
    for (command, expected) in table {
        let outcome = shell(&engine, command);
        assert_eq!(
            said(&outcome),
            *expected,
            "{command:?}: {}",
            reason(&outcome)
        );
    }
}

#[test]
fn a_denial_and_a_question_say_why() {
    let engine = with_rules(vec![user(Effect::Exec, "rm *", RuleAction::Deny)]);
    let denied = reason(&shell(&engine, "rm -rf x"));
    assert!(
        denied.contains("`deny exec rm *` from your configuration"),
        "{denied}"
    );

    let asked = reason(&shell(&engine, "cargo build"));
    assert!(
        asked.contains("`cargo build` is not on a safe list"),
        "{asked}"
    );
    let asked = reason(&shell(&engine, "cat $(id)"));
    assert!(
        asked.contains("`$`") && asked.contains("does not model"),
        "{asked}"
    );
    let asked = reason(&shell(&engine, "cat /etc/passwd"));
    assert!(
        asked.contains("`/etc/passwd`") && asked.contains("outside the workspace"),
        "{asked}"
    );
    let asked = reason(&shell(&engine, "cat .env"));
    assert!(asked.contains("holds secrets"), "{asked}");
    let refused = shell(&engine, "cat ~/.config/cortex-harness/credentials.toml");
    assert_eq!(said(&refused), Deny);
    assert!(reason(&refused).contains("built-in rule"), "{refused:?}");
    let searching = with_rules(vec![user(Effect::Read, "secrets/**", RuleAction::Deny)]);
    let asked = reason(&shell(&searching, "rg KEY"));
    assert!(
        asked.contains("reads every file it finds in `/work/app`")
            && asked.contains("`deny read secrets/**`")
            && asked.contains("`search` tool"),
        "{asked}"
    );
    let asked = reason(&shell(&engine, "cd src; ls"));
    assert!(asked.contains("cannot follow"), "{asked}");
    let asked = reason(&shell(&engine, "echo x > .git/hooks/pre-commit"));
    assert!(asked.contains("protected path"), "{asked}");
}

/// What "allow for this session" would add is the program and its
/// subcommand, and it is offered only where a rule would settle every
/// reason the call was asked about.
#[test]
fn a_question_about_a_command_offers_the_rule_an_approval_would_add() {
    let engine = engine();
    let lifts = |command: &str| -> Vec<String> {
        question(shell(&engine, command))
            .lifts
            .iter()
            .map(|rule| format!("{} {}", rule.effect.as_str(), rule.pattern))
            .collect()
    };
    assert_eq!(lifts("cargo build --release"), ["exec cargo build *"]);
    assert_eq!(
        lifts("npm run build && npm test"),
        ["exec npm run *", "exec npm test *"]
    );
    assert_eq!(lifts("ls && make -j8"), ["exec make *"]);
    assert_eq!(lifts("sudo apt list"), ["exec sudo apt list"]);
    // A program with a language of its own in its arguments, and one whose
    // subcommand is not where it is expected: the line that was shown.
    assert_eq!(
        lifts("find . -name x -delete"),
        ["exec find . -name x -delete"]
    );
    assert_eq!(
        lifts("git -C sub commit -m x"),
        ["exec git -C sub commit -m x"]
    );
    assert_eq!(lifts("git commit -m x"), ["exec git commit *"]);
    // A glob takes `ls` off the table of commands that only read. A rule
    // the person is shown, and chooses, can still allow it.
    assert_eq!(lifts("ls *.rs"), ["exec ls *"]);
    // A read outside the workspace: that one path.
    assert_eq!(lifts("cat /etc/hosts"), ["read /etc/hosts"]);
    assert_eq!(lifts("echo x > /tmp/out.txt"), ["write /tmp/out.txt"]);
    // Nothing a rule could say safely: allow once, or deny.
    for command in [
        "cat $(id)",
        "FOO=1 make",
        "bash -c 'rm -rf x'",
        "cd src; ls",
        "cargo build && echo $HOME",
    ] {
        let question = question(shell(&engine, command));
        assert!(
            question.lifts.is_empty(),
            "{command:?}: {:?}",
            question.lifts
        );
        assert_eq!(question.choices().len(), 2, "{command:?}");
    }
}

/// A command run from a directory outside the workspace is asked about,
/// read-only or not: it may write there, and it reads there.
#[test]
fn a_command_run_outside_the_workspace_is_asked_about() {
    let engine = with_rules(vec![user(Effect::Exec, "make *", RuleAction::Allow)]);
    assert_eq!(said(&shell_in(&engine, "ls", "/work/app/src")), ReadOnly);
    assert_eq!(said(&shell_in(&engine, "ls", "/tmp")), Ask);
    assert_eq!(said(&shell_in(&engine, "make", "/work/app/sub")), Allow);
    assert_eq!(said(&shell_in(&engine, "make", "/srv/other")), Ask);
    assert_eq!(said(&shell_in(&engine, "make", "/users/dev")), Ask);
    // The session's own temporary directory is inside the scope.
    assert_eq!(said(&shell_in(&engine, "make", "/tmp/cortex-s1")), Allow);
}

/// `git` reads its repository's own configuration, which can name programs
/// to run. So it is on the table only in a project the user has trusted.
#[test]
fn git_is_asked_about_in_a_project_that_is_not_trusted() {
    let untrusted = engine_with(|parts| parts.trusted = false);
    for command in [
        "git status",
        "git diff",
        "git log --oneline",
        "ls && git status",
    ] {
        assert_eq!(said(&shell(&untrusted, command)), Ask, "{command:?}");
    }
    assert_eq!(said(&shell(&untrusted, "ls -la")), ReadOnly);
    assert_eq!(said(&shell(&engine(), "git status")), ReadOnly);
}

/// A project's allow for a command applies only once the project is
/// trusted, and its deny applies at once.
#[test]
fn a_projects_command_rules_tighten_at_once_and_loosen_only_with_trust() {
    let rules = || {
        vec![
            project(Effect::Exec, "make deploy", RuleAction::Allow),
            project(Effect::Exec, "cargo *", RuleAction::Allow),
            project(Effect::Exec, "cargo publish *", RuleAction::Deny),
            user(Effect::Exec, "cargo test *", RuleAction::Allow),
        ]
    };
    let untrusted = engine_with(|parts| {
        parts.trusted = false;
        parts.rules = rules();
    });
    assert_eq!(said(&shell(&untrusted, "make deploy")), Ask);
    assert_eq!(said(&shell(&untrusted, "cargo build")), Ask);
    assert_eq!(
        said(&shell(&untrusted, "cargo test")),
        Allow,
        "the user's own rule"
    );
    assert_eq!(said(&shell(&untrusted, "cargo publish")), Deny);
    let withheld: Vec<String> = untrusted
        .withheld()
        .iter()
        .map(|rule| rule.show())
        .collect();
    assert_eq!(withheld, ["allow exec make deploy", "allow exec cargo *"]);

    let trusted = engine_with(|parts| parts.rules = rules());
    assert_eq!(said(&shell(&trusted, "make deploy")), Allow);
    assert_eq!(said(&shell(&trusted, "cargo build")), Allow);
    assert_eq!(said(&shell(&trusted, "cargo publish")), Deny);
    assert!(trusted.withheld().is_empty());
}

/// PowerShell: one command with literal arguments is analysed, and
/// everything else is asked about.
#[test]
fn the_command_table_for_powershell() {
    let engine = engine_with(|parts| {
        parts.shell = ShellSyntax::PowerShell;
        parts.rules = vec![
            user(Effect::Exec, "cargo test *", RuleAction::Allow),
            user(Effect::Exec, "Remove-Item *", RuleAction::Deny),
        ];
    });
    let table: &[(&str, Said)] = &[
        ("Get-ChildItem", ReadOnly),
        ("Get-ChildItem -Recurse src", ReadOnly),
        ("gci -Path src -Filter x.rs", ReadOnly),
        ("Get-Content README.md", ReadOnly),
        ("get-content -TotalCount 5 README.md", ReadOnly),
        ("Select-String -Pattern TODO -Path README.md", ReadOnly),
        ("Test-Path Cargo.toml", ReadOnly),
        ("Get-Location", ReadOnly),
        ("git status", ReadOnly),
        ("echo 'hello there'", ReadOnly),
        ("cargo test", Allow),
        ("Cargo test --workspace", Allow),
        ("cargo build", Ask),
        ("Set-Content x y", Ask),
        ("Get-Content -Wait log.txt", Ask),
        ("Get-Content .env", Ask),
        ("Get-Content /etc/passwd", Ask),
        (
            "Get-Content ~/.config/cortex-harness/credentials.toml",
            Deny,
        ),
        // A pattern in quotes: PowerShell's commands expand it themselves.
        ("Get-Content '.en?'", Ask),
        ("Get-Content '*.env'", Ask),
        ("Select-String -Pattern KEY -Path '*.pem'", Ask),
        // Not a file: the environment, the registry.
        ("Get-ChildItem Env:", Ask),
        ("Get-Content Env:PATH", Ask),
        ("Get-Item HKLM:\\Software", Ask),
        ("Get-ChildItem ..", Ask),
        // More than one command, and every way to hide one.
        ("Get-ChildItem; Remove-Item x", Deny),
        ("Get-ChildItem | Remove-Item", Deny),
        ("Get-ChildItem; cargo build", Ask),
        ("Get-Content x | Out-File y", Ask),
        ("cargo test; cargo build", Ask),
        ("cargo test && cargo build", Ask),
        ("& cargo test", Ask),
        ("cargo test $args", Ask),
        ("cargo test (Get-Date)", Ask),
        ("cargo test { x }", Ask),
        ("cargo test `n", Ask),
        ("cargo test > out.txt", Ask),
        ("cargo test @args", Ask),
        ("cargo test \"$env:X\"", Ask),
        ("powershell -EncodedCommand AAAA", Ask),
        ("pwsh -enc AAAA", Ask),
        ("powershell.exe -e AAAA", Ask),
        ("cmd /c dir", Ask),
        ("Invoke-Expression 'cargo test'", Ask),
        ("iex 'cargo test'", Ask),
        // A deny matches however the command is written.
        ("Remove-Item x", Deny),
        ("remove-item -Recurse x", Deny),
        ("powershell -Command Remove-Item x", Deny),
        ("iex 'Remove-Item x'", Deny),
        ("& Remove-Item x", Deny),
        // A program named by a path is not the one the table means.
        (".\\git.exe status", Ask),
        ("C:\\evil\\git.exe status", Ask),
    ];
    for (command, expected) in table {
        let outcome = shell(&engine, command);
        assert_eq!(
            said(&outcome),
            *expected,
            "{command:?}: {}",
            reason(&outcome)
        );
    }
}

/// `cmd.exe`, and a shell nobody has taught the analysis: nothing is
/// modelled, so every command is asked about, no allow rule applies, and a
/// deny still does.
#[test]
fn the_command_table_for_cmd_and_for_a_shell_that_is_not_known() {
    for shell_syntax in [ShellSyntax::Cmd, ShellSyntax::Unknown] {
        let engine = engine_with(|parts| {
            parts.shell = shell_syntax;
            parts.rules = vec![
                user(Effect::Exec, "dir *", RuleAction::Allow),
                user(Effect::Exec, "*", RuleAction::Allow),
                user(Effect::Exec, "del *", RuleAction::Deny),
                user(Effect::Exec, "git push *", RuleAction::Ask),
            ];
        });
        let table: &[(&str, Said)] = &[
            ("dir", Ask),
            ("dir /s", Ask),
            ("ls", Ask),
            ("echo hello", Ask),
            ("type README.md", Ask),
            ("git status", Ask),
            ("cargo test", Ask),
            ("dir & del x", Deny),
            ("del /q x", Deny),
            ("DEL.EXE x", Deny),
            ("cmd /c \"del x\"", Deny),
            ("git push", Ask),
        ];
        for (command, expected) in table {
            let outcome = shell(&engine, command);
            assert_eq!(
                said(&outcome),
                *expected,
                "{command:?} in {shell_syntax:?}: {}",
                reason(&outcome)
            );
        }
        let why = reason(&shell(&engine, "dir"));
        assert!(why.contains("not analysed"), "{why}");
        assert!(question(shell(&engine, "dir")).lifts.is_empty());
    }
    // At a level that lets every command run, they run: the level, not a
    // rule, is what allows them.
    let unrestricted = engine_with(|parts| {
        parts.shell = ShellSyntax::Cmd;
        parts.level = Level::Unrestricted;
        parts.rules = vec![user(Effect::Exec, "del *", RuleAction::Deny)];
    });
    assert_eq!(said(&shell(&unrestricted, "dir /s")), Allow);
    assert_eq!(said(&shell(&unrestricted, "del x")), Deny);
    let _ = at;
}
