//! The read-only table, command by command: what is on it, what each
//! command names, and every way off it.

use super::*;
use crate::command::{Analysis, analyse};

fn part(command: &str, syntax: ShellSyntax) -> Option<Part> {
    match analyse(command, syntax) {
        Analysis::Parts(mut parts) if parts.len() == 1 => Some(parts.remove(0)),
        _ => None,
    }
}

/// The paths a command names when it is on the table, in a trusted
/// project. `None` when it is not on the table, or is not one modelled
/// command.
fn named(command: &str, syntax: ShellSyntax) -> Option<Vec<String>> {
    let part = part(command, syntax)?;
    read_only(&part, syntax, true).map(|named| named.into_iter().map(|named| named.text).collect())
}

fn posix(command: &str) -> Option<Vec<String>> {
    named(command, ShellSyntax::Posix)
}

#[test]
fn commands_that_only_read_are_on_the_table_with_what_they_name() {
    let cases: &[(&str, &[&str])] = &[
        ("ls", &[]),
        ("ls -la", &[]),
        ("ls -la src tests", &["src", "tests"]),
        ("ls --color=auto -lh --sort=time src", &["src"]),
        ("ls -I target src", &["src"]),
        ("ls -- -odd-name", &["-odd-name"]),
        ("cat README.md", &["README.md"]),
        (
            "cat -n src/main.rs Cargo.toml",
            &["src/main.rs", "Cargo.toml"],
        ),
        ("cat -", &[]),
        ("cat ~/notes.txt", &["~/notes.txt"]),
        ("head -n 20 a.txt", &["a.txt"]),
        ("head -20 a.txt", &["a.txt"]),
        ("head -n20 a.txt", &["a.txt"]),
        ("head --lines=20 a.txt", &["a.txt"]),
        ("head --lines 20 a.txt", &["a.txt"]),
        ("tail -5 log.txt", &["log.txt"]),
        ("tail -n +5 log.txt", &["log.txt"]),
        ("wc -l src/main.rs", &["src/main.rs"]),
        ("grep -rn TODO src", &["TODO", "src"]),
        ("grep -rn 'a b' src", &["a b", "src"]),
        ("grep -e pattern -A 2 file.txt", &["file.txt"]),
        ("grep --include='*.rs' -r x .", &["x", "."]),
        ("grep -c x -- -file", &["x", "-file"]),
        ("egrep 'a|b' f", &["a|b", "f"]),
        ("rg -n TODO src", &["TODO", "src"]),
        ("rg --files -g '*.rs'", &[]),
        ("rg -t rust -C3 'fn main' crates", &["fn main", "crates"]),
        ("fd -e rs main src", &["main", "src"]),
        ("find . -name '*.rs' -type f", &["."]),
        (
            "find src tests -maxdepth 2 -newer Cargo.toml -print",
            &["src", "tests", "Cargo.toml"],
        ),
        ("find -name x", &[]),
        ("find . -type f -not -name '*.md' -o -empty", &["."]),
        ("sort -u -k2,3 -t, data.csv", &["data.csv"]),
        ("sort --key=2 data.csv", &["data.csv"]),
        ("uniq -c data.txt", &["data.txt"]),
        ("stat -c %s Cargo.toml", &["Cargo.toml"]),
        ("file -b target/debug/cortex", &["target/debug/cortex"]),
        ("du -sh target", &["target"]),
        ("du -d 1 .", &["."]),
        ("df -h", &[]),
        ("tree -L 2 src", &["src"]),
        ("readlink -f link", &["link"]),
        (
            "realpath --relative-to=src src/main.rs",
            &["src", "src/main.rs"],
        ),
        ("basename src/main.rs .rs", &[]),
        ("dirname src/main.rs", &[]),
        ("pwd", &[]),
        ("pwd -P", &[]),
        ("whoami", &[]),
        ("id -un", &[]),
        ("uname -a", &[]),
        ("hostname", &[]),
        ("hostname -f", &[]),
        ("which cargo rustc", &[]),
        ("true", &[]),
        ("false", &[]),
        ("echo hello -n --anything 'at all'", &[]),
        ("echo ~/x", &[]),
        ("printf '%s\\n' a b", &[]),
        ("test -f Cargo.toml", &[]),
        ("date", &[]),
        ("date -u +%Y-%m-%d", &[]),
        ("date -Iseconds", &[]),
        ("date --iso-8601=seconds", &[]),
        ("sed -n '10,20p' src/main.rs", &["src/main.rs"]),
        ("sed -n 5p a.txt b.txt", &["a.txt", "b.txt"]),
        ("sed -n '3,$p' a.txt", &["a.txt"]),
        ("sed --quiet 7p", &[]),
        // git, in a trusted project.
        ("git status", &[]),
        ("git status --short --branch", &[]),
        ("git status -sb -uno", &[]),
        ("git status --porcelain=v2 src", &["src"]),
        ("git diff", &[]),
        ("git diff --stat HEAD~1", &["HEAD~1"]),
        ("git diff --cached -- src/main.rs", &["src/main.rs"]),
        ("git diff main..feature --name-only", &["main..feature"]),
        ("git diff -U5 --no-ext-diff a b", &["a", "b"]),
        ("git log", &[]),
        ("git log --oneline -20", &[]),
        ("git log -n 5 --format=%H --since=2.weeks -- src", &["src"]),
        ("git log -p --follow src/lib.rs", &["src/lib.rs"]),
        ("git show HEAD", &["HEAD"]),
        (
            "git show HEAD:src/main.rs",
            &["HEAD:src/main.rs", "src/main.rs"],
        ),
        ("git show --stat abc123", &["abc123"]),
        ("git branch", &[]),
        ("git branch -a -vv", &[]),
        ("git branch --show-current", &[]),
        ("git branch --contains HEAD", &[]),
        ("git tag", &[]),
        ("git tag -l", &[]),
        ("git rev-parse --abbrev-ref HEAD", &[]),
        ("git rev-parse --show-toplevel", &[]),
        ("git ls-files -m src", &["src"]),
        ("git blame -L 10,20 src/main.rs", &["src/main.rs"]),
        ("git describe --tags --always", &[]),
        ("git grep -n TODO -- src", &["TODO", "src"]),
        ("git remote -v", &[]),
        ("git stash list", &[]),
        ("git version", &[]),
        ("git --version", &[]),
    ];
    for (command, expected) in cases {
        assert_eq!(
            posix(command),
            Some(expected.iter().map(|path| (*path).to_owned()).collect()),
            "{command:?}"
        );
    }
}

/// Every way off the table. Each of these writes, runs something, never
/// ends, or cannot be known not to.
#[test]
fn a_command_with_anything_unlisted_is_not_on_the_table() {
    for command in [
        // Options that write.
        "sort -o out.txt data.txt",
        "sort --output=out.txt data.txt",
        "sort --output out.txt data.txt",
        "sort -uo out.txt data.txt",
        "sort -T /tmp data.txt",
        "uniq in.txt out.txt",
        "tree -o out.txt",
        "find . -delete",
        "find . -name x -delete",
        "find . -fprint out.txt",
        "find . -fprintf out.txt %p",
        "find . -fls out.txt",
        "git diff --output=patch.diff",
        "git diff --output patch.diff",
        "git log --output=x",
        "git show --output=x HEAD",
        "sed -i 's/a/b/' f",
        "sed -n -i 5p f",
        "sed -n 5p -i f",
        "sed 's/a/b/w out' f",
        "sed -n '1w out' f",
        "sed -n 5p --in-place f",
        "date -s 2020-01-01",
        "date --set=2020-01-01",
        "hostname evil",
        "file -C -m magic",
        // Options and forms that run a program.
        "find . -exec rm x ;",
        "find . -execdir sh -c x ;",
        "find . -ok rm x ;",
        "find . -okdir rm x ;",
        "sort --compress-program=sh data.txt",
        "rg --pre cat x",
        "rg --pre=cat x",
        "rg --hostname-bin=/tmp/x x",
        "rg -z x",
        "fd -x rm",
        "fd --exec rm",
        "fd -X rm",
        "fd --exec-batch rm",
        "fd -l",
        "git grep -O less x",
        "git grep --open-files-in-pager=less x",
        "git diff --ext-diff",
        "git diff --textconv",
        "git log --ext-diff -p",
        "git -c core.pager=evil log",
        "git -c alias.x=!evil x",
        "git -C /tmp status",
        "git --git-dir=/tmp/x status",
        "git --exec-path=/tmp status",
        "git -p log",
        "sed -n '1e id' f",
        "sed 's/a/b/e' f",
        "sed -n '1!p' f",
        "sed -n p f",
        "sed -n '5q' f",
        "sed -e 5p f",
        "sed -n -e 5p f",
        "sed -n",
        "sed",
        // Subcommands that write or reach the network.
        "git branch new-branch",
        "git branch -d old",
        "git branch -D old",
        "git branch -m a b",
        "git branch --list 'feat*' extra",
        "git tag v1.0",
        "git tag -d v1.0",
        "git remote add origin url",
        "git remote show origin",
        "git remote update",
        "git stash",
        "git stash pop",
        "git stash drop",
        "git stash list extra",
        "git push",
        "git pull",
        "git fetch",
        "git commit -m x",
        "git add .",
        "git checkout main",
        "git reset --hard",
        "git clean -fd",
        "git config user.name x",
        "git config --get user.name",
        "git gc",
        "git",
        "git status --no-such-option",
        "git log --outpu=x",
        // An abbreviation of a long option is not the option listed.
        "sort --rev data.txt",
        "ls --al",
        "grep --rec x .",
        // Options that follow links out of the tree, and `diff`.
        "grep -R x .",
        "grep --dereference-recursive x .",
        "rg -L x",
        "rg --follow x",
        "fd -L x",
        "find -L . -name x",
        "find . -follow",
        "ls -L",
        "ls --dereference",
        "tree -l",
        "du -L .",
        "diff a b",
        "diff -r a b",
        // Commands that never end.
        "tail -f log.txt",
        "tail --follow log.txt",
        "tail -F log.txt",
        // Programs that are languages, write, or run others.
        "awk '{print $1}' f",
        "perl -ne print f",
        "python -c 'print(1)'",
        "python --version",
        "node --version",
        "cargo --version",
        "xargs ls",
        "env",
        "env ls",
        "printenv",
        "tee out.txt",
        "less README.md",
        "more README.md",
        "man ls",
        "vi x",
        "cp a b",
        "mv a b",
        "rm a",
        "mkdir d",
        "touch f",
        "chmod +x f",
        "ln -s a b",
        "curl https://example.com",
        "wget https://example.com",
        "make",
        "cargo test",
        "npm test",
        "sh -c ls",
        "bash -c ls",
        "sudo ls",
        "timeout 5 ls",
        "nohup ls",
        "nice ls",
        "watch ls",
        "ssh host ls",
        "cd src",
        // The program must be named plainly.
        "./ls",
        "/bin/ls",
        "/tmp/ls -la",
        "bin/cat x",
        "LS",
        "Cat x",
        "'ls'x",
        // Anything the shell would expand.
        "ls *.rs",
        "cat src/*.rs",
        "wc -l *",
        "grep x src/?.rs",
        "ls [a-c]",
        "echo *",
        "git diff -- *.rs",
        "sort -k *",
        // A value that is missing.
        "head -n",
        "grep -e",
        "sort --key",
        "find . -name",
        "find . -newer",
        "head --lines",
        // A value where none is taken.
        "ls --all=yes",
        "git status --short=x",
        // Unknown short options, alone or in a cluster.
        "ls -la --frobnicate",
        "ls -lZzz",
        "cat -o x",
        "wc --files0-from=list",
        "grep -f patterns x",
        "grep --file=patterns x",
        "grep -d recurse x",
        "date -r file",
        "date -f file",
        "date +%Y +%m",
        "date tomorrow",
        "pwd extra",
        "whoami extra",
        "uname extra",
        "id a b",
    ] {
        let Some(part) = part(command, ShellSyntax::Posix) else {
            // Not one modelled command: it never reaches the table.
            continue;
        };
        assert_eq!(
            read_only(&part, ShellSyntax::Posix, true),
            None,
            "{command:?} is on the table"
        );
    }
}

/// The cases above that the tokenizer already refuses must stay refused:
/// if one became modelled, the loop above would start testing it, and this
/// list says which were expected never to get that far.
#[test]
fn some_forms_never_reach_the_table() {
    for command in [
        "find . -exec rm {} ;",
        "find . -exec rm {} +",
        "find . -name x ! -name y",
        "find . ( -name x )",
        "awk '{print $1}' f | sh",
        "cat $(echo x)",
        "ls > out.txt; ls",
    ] {
        let reaches = part(command, ShellSyntax::Posix)
            .is_some_and(|part| read_only(&part, ShellSyntax::Posix, true).is_some());
        assert!(!reaches, "{command:?}");
    }
}

#[test]
fn git_is_on_the_table_only_in_a_trusted_project() {
    for command in [
        "git status",
        "git diff",
        "git log --oneline",
        "git --version",
    ] {
        let part = part(command, ShellSyntax::Posix).unwrap();
        assert!(
            read_only(&part, ShellSyntax::Posix, true).is_some(),
            "{command:?}"
        );
        assert_eq!(
            read_only(&part, ShellSyntax::Posix, false),
            None,
            "{command:?} without trust"
        );
    }
    // Trust changes nothing for the rest of the table.
    let part = part("ls -la", ShellSyntax::Posix).unwrap();
    assert!(read_only(&part, ShellSyntax::Posix, false).is_some());
    assert!(read_only(&part, ShellSyntax::Posix, true).is_some());
}

#[test]
fn a_named_revision_need_not_be_a_path_and_a_named_file_must() {
    let named_of = |command: &str| {
        let part = part(command, ShellSyntax::Posix).unwrap();
        read_only(&part, ShellSyntax::Posix, true).unwrap()
    };
    assert!(named_of("cat a.txt").iter().all(|named| named.must_resolve));
    assert!(
        named_of("find src -newer x")
            .iter()
            .all(|named| named.must_resolve)
    );
    assert!(
        named_of("git show HEAD:a.txt")
            .iter()
            .all(|named| !named.must_resolve)
    );
    assert!(
        named_of("git status src")
            .iter()
            .all(|named| named.must_resolve)
    );
}

#[test]
fn the_table_is_per_shell_and_empty_for_shells_that_are_not_analysed() {
    assert!(names(ShellSyntax::Posix).contains(&"ls"));
    assert!(names(ShellSyntax::Posix).contains(&"git"));
    assert!(names(ShellSyntax::PowerShell).contains(&"get-childitem"));
    assert!(!names(ShellSyntax::PowerShell).contains(&"sed"));
    assert!(names(ShellSyntax::Cmd).is_empty());
    assert!(names(ShellSyntax::Unknown).is_empty());
    let ls = Part {
        joiner: crate::command::Joiner::First,
        words: vec![Word::literal("ls")],
        redirects: Vec::new(),
    };
    assert!(read_only(&ls, ShellSyntax::Posix, true).is_some());
    assert_eq!(read_only(&ls, ShellSyntax::Cmd, true), None);
    assert_eq!(read_only(&ls, ShellSyntax::Unknown, true), None);
    // No name is on a table twice, which would make one row unreachable.
    for syntax in [ShellSyntax::Posix, ShellSyntax::PowerShell] {
        let mut all = names(syntax);
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count, "{syntax:?}");
    }
}

fn powershell(command: &str) -> Option<Vec<String>> {
    named(command, ShellSyntax::PowerShell)
}

#[test]
fn powershell_commands_that_only_read_are_on_the_table() {
    let cases: &[(&str, &[&str])] = &[
        ("Get-ChildItem", &[]),
        ("get-childitem -Recurse -Force src", &["src"]),
        ("gci -Path src -Filter x.rs -Depth 2", &["src"]),
        ("ls src", &["src"]),
        ("dir", &[]),
        ("Get-Content README.md", &["README.md"]),
        ("Get-Content -Path README.md -TotalCount 5", &["README.md"]),
        ("gc -Tail 20 -LiteralPath 'my log.txt'", &["my log.txt"]),
        ("cat README.md", &["README.md"]),
        ("type README.md", &["README.md"]),
        ("Get-Content -Path:README.md -Raw", &["README.md"]),
        (
            "Select-String -Pattern TODO -Path src\\main.rs",
            &["src\\main.rs"],
        ),
        ("sls TODO README.md", &["TODO", "README.md"]),
        ("Test-Path Cargo.toml", &["Cargo.toml"]),
        ("Test-Path -Path Cargo.toml -PathType Leaf", &["Cargo.toml"]),
        ("Resolve-Path src", &["src"]),
        ("Get-Item Cargo.toml", &["Cargo.toml"]),
        ("Get-Location", &[]),
        ("pwd", &[]),
        ("Get-Date", &[]),
        ("Get-Date -Format yyyy-MM-dd", &[]),
        ("Write-Output hello world", &[]),
        ("echo 'hello there'", &[]),
        ("echo '-NotAParameter'", &[]),
        ("Write-Host -NoNewline hi", &[]),
        ("Get-FileHash Cargo.lock -Algorithm SHA256", &["Cargo.lock"]),
        ("whoami", &[]),
        ("hostname", &[]),
        ("git status --short", &[]),
        ("GIT.EXE log --oneline -5", &[]),
        ("rg -n TODO src", &["TODO", "src"]),
    ];
    for (command, expected) in cases {
        assert_eq!(
            powershell(command),
            Some(expected.iter().map(|path| (*path).to_owned()).collect()),
            "{command:?}"
        );
    }
}

#[test]
fn a_powershell_command_with_anything_unlisted_is_not_on_the_table() {
    for command in [
        // Parameters that are not listed, their abbreviations included.
        "Get-Content -Wait log.txt",
        "Get-Content -Stream Zone.Identifier x",
        "Get-Content -Rec x",
        "Get-ChildItem -Rec",
        "Get-ChildItem -OutVariable x",
        "Get-ChildItem -ErrorAction Stop",
        "Get-ChildItem -Recurse:true",
        "Get-Content -Path",
        "Get-Content -Path:",
        "Get-Date -Wrong",
        "Get-Location extra",
        // Unix options given to a PowerShell alias.
        "ls -la",
        "cat -n x",
        "echo -n hi",
        // Patterns, which the command expands itself.
        "Get-Content *.env",
        "ls src\\*.rs",
        // Commands that write, run or change something.
        "Set-Content x y",
        "Out-File x",
        "Remove-Item x",
        "rm x",
        "Copy-Item a b",
        "New-Item x",
        "Start-Process x",
        "Invoke-WebRequest example.com",
        "Set-Location src",
        "cd src",
        "Get-Command git",
        "Get-Process",
        "Import-Module x",
        "Add-Type x",
        "sort x",
        "tee x",
        "sed -n 5p x",
        "find . -name x",
        // A program named by a path.
        ".\\git.exe status",
        "C:\\evil\\git.exe status",
        "./git status",
        "whoami /all",
        "hostname x",
        "git push",
        "git -c x=y status",
    ] {
        let Some(part) = part(command, ShellSyntax::PowerShell) else {
            continue;
        };
        assert_eq!(
            read_only(&part, ShellSyntax::PowerShell, true),
            None,
            "{command:?} is on the table"
        );
    }
    // Without trust, git is off the PowerShell table too.
    let part = part("git status", ShellSyntax::PowerShell).unwrap();
    assert_eq!(read_only(&part, ShellSyntax::PowerShell, false), None);
}
