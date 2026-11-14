//! The guard before a push, with made-up terms. The owner's own file is
//! never read: every test names a terms file of its own, and a home
//! directory of its own.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::{Finding, GIT_LOCATION, Terms};
use super::{
    Found, Loaded, TERMS_FILE, TermsFile, Update, by_commit, check, load, names_by_commit,
    parse_updates, screened,
};

/// Made-up terms of the kinds the owner's file holds: a name, a phrase, an
/// email and a home path.
const TERMS: &str = "Quentin\nExample Person\nq.example@example.invalid\n/srv/qexample\n";

fn terms() -> Terms {
    Terms::parse(TERMS)
}

/// A folder of the test's own, empty, in the system's temporary folder.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xtask-local-guard-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}

#[test]
fn the_terms_are_found_where_the_variable_says_or_under_the_home_folder() {
    let named = TermsFile::find(env(&[(TERMS_FILE, "/x/terms"), ("HOME", "/h")]));
    assert_eq!(named, TermsFile::Named(PathBuf::from("/x/terms")));
    let default = TermsFile::find(env(&[("HOME", "/h")]));
    assert_eq!(
        default,
        TermsFile::Default(PathBuf::from("/h/.config/cortex-harness/owner-terms"))
    );
    // Windows has USERPROFILE where Unix has HOME; an empty value is none.
    let windows = TermsFile::find(env(&[
        ("HOME", ""),
        ("USERPROFILE", "/u"),
        (TERMS_FILE, ""),
    ]));
    assert_eq!(
        windows,
        TermsFile::Default(PathBuf::from("/u/.config/cortex-harness/owner-terms"))
    );
    assert_eq!(TermsFile::find(env(&[])), TermsFile::Nowhere);
}

#[test]
fn without_the_default_file_the_push_goes_on_and_a_named_file_must_be_there() {
    let dir = scratch("missing");
    let none = || -> Vec<PathBuf> { Vec::new() };
    let default = TermsFile::Default(dir.join("owner-terms"));
    let Loaded::Missing(why) = load(&default, none).unwrap() else {
        panic!("a missing default file is a warning");
    };
    assert!(
        why.contains("~/.config/cortex-harness/owner-terms"),
        "{why}"
    );
    assert!(matches!(
        load(&TermsFile::Nowhere, none),
        Ok(Loaded::Missing(_))
    ));

    let named = TermsFile::Named(dir.join("owner-terms"));
    let refused = load(&named, none).unwrap_err();
    assert!(refused.contains("refused"), "{refused}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_is_there_and_cannot_be_used_refuses_the_push() {
    // The folder's own path holds a term: no message may show it.
    let dir = scratch("unusable").join("srv").join("qexample");
    std::fs::create_dir_all(&dir).unwrap();
    let none = || -> Vec<PathBuf> { Vec::new() };
    let file = dir.join("owner-terms");
    let messages = |file: &TermsFile| {
        let message = load(file, none).unwrap_err();
        assert!(message.contains("refused"), "{message}");
        assert!(!terms().matches(&message), "{message}");
        assert!(!message.contains("qexample"), "{message}");
        message
    };

    std::fs::write(&file, "\n  \n").unwrap();
    assert!(messages(&TermsFile::Default(file.clone())).contains("holds no term"));
    std::fs::write(&file, [0xffu8, 0xfe, b'\n']).unwrap();
    assert!(messages(&TermsFile::Named(file.clone())).contains("not UTF-8"));
    // A folder where the file should be cannot be read as one.
    let folder = dir.join("a-folder");
    std::fs::create_dir_all(&folder).unwrap();
    assert!(messages(&TermsFile::Default(folder)).contains("cannot be read"));

    // Inside the repository, where `git add` could take it.
    std::fs::write(&file, TERMS).unwrap();
    let inside = load(&TermsFile::Default(file.clone()), || vec![dir.clone()]).unwrap_err();
    assert!(inside.contains("inside the repository"), "{inside}");
    assert!(!inside.contains("qexample"), "{inside}");
    let outside = load(&TermsFile::Default(file.clone()), || {
        vec![dir.join("elsewhere")]
    });
    assert!(matches!(outside, Ok(Loaded::Terms(_))));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
}

#[test]
fn the_updates_git_gives_a_hook_are_read_and_anything_else_refused() {
    let a = "a".repeat(40);
    let zero = "0".repeat(40);
    let text = format!("refs/heads/x {a} refs/heads/x {zero}\n\nrefs/tags/t {a} refs/tags/t {a}\n");
    let updates = parse_updates(&text).unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(
        updates[0],
        Update {
            local_ref: "refs/heads/x".to_owned(),
            local: a.clone(),
            remote_ref: "refs/heads/x".to_owned(),
            remote: zero,
        }
    );
    assert!(parse_updates("refs/heads/x abc\n").is_err());
    assert!(parse_updates(&format!("x {a} y not-an-object\n")).is_err());
    assert_eq!(parse_updates("").unwrap(), []);
}

#[test]
fn a_log_is_split_into_each_commits_diff_and_names() {
    let one = "1".repeat(40);
    let two = "2".repeat(40);
    let log = format!(
        "\u{1e}{one}\n\ndiff --git a/x b/x\n+++ b/x\n@@ -0,0 +1 @@\n+line\n\u{1e}{two}\n\u{1e}{one}x\n"
    );
    let commits = by_commit(&log);
    assert_eq!(commits.len(), 2, "{commits:?}");
    assert_eq!(commits[0].0, "1".repeat(12));
    assert!(commits[0].1.contains("+line\n"));
    // A line that only looks like a marker is part of the diff before it.
    assert!(commits[1].1.contains(&format!("{one}x")));

    let names = format!("\u{1e}{one}\0\na b\0c\0\u{1e}{two}\0\u{1e}{one}\0\n\nd\0");
    let commits = names_by_commit(&names);
    assert_eq!(
        commits,
        [
            ("1".repeat(12), "a b\0c\0".to_owned()),
            ("2".repeat(12), String::new()),
            // A name that begins with a line break keeps it.
            ("1".repeat(12), "\nd\0".to_owned()),
        ]
    );
}

#[test]
fn what_is_printed_never_holds_a_term() {
    let terms = terms();
    assert_eq!(
        screened(
            Some(&terms),
            "fatal: /srv/qexample/repo is broken".to_owned()
        ),
        "a message that matches a protected term was withheld"
    );
    assert_eq!(screened(Some(&terms), "plain".to_owned()), "plain");
    let found = [
        Found::InCommit {
            commit: "c".repeat(12),
            finding: Finding::Path { file: 3 },
        },
        Found::InCommit {
            commit: "c".repeat(12),
            finding: Finding::Line {
                path: Some("a.txt".to_owned()),
                line: 2,
            },
        },
        Found::Ref {
            line: 1,
            side: "remote",
        },
        Found::Tag {
            object: "d".repeat(12),
        },
    ];
    for finding in found {
        let text = finding.to_string();
        assert!(text.ends_with("matches a protected term"), "{text}");
        assert!(!terms.matches(&text), "{text}");
    }
}

/// git in `dir`, with no configuration of the machine's, as `who`.
fn git_in(dir: &Path, config: &Path, who: &str, arguments: &[&str]) -> String {
    let output = git_command(dir, config, who)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn git_command(dir: &Path, config: &Path, who: &str) -> Command {
    let mut command = Command::new("git");
    for variable in GIT_LOCATION {
        command.env_remove(variable);
    }
    for (variable, value) in [
        ("GIT_AUTHOR_NAME", who),
        ("GIT_COMMITTER_NAME", who),
        ("GIT_AUTHOR_EMAIL", "x@example.invalid"),
        ("GIT_COMMITTER_EMAIL", "x@example.invalid"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ] {
        command.env(variable, value);
    }
    command
        .env("GIT_CONFIG_GLOBAL", config)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .current_dir(dir);
    command
}

/// A repository with a remote that has its first commit.
struct Repo {
    dir: PathBuf,
    config: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let root = scratch(name);
        let config = root.join("gitconfig");
        std::fs::write(&config, "").unwrap();
        let dir = root.join("work");
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Self { dir, config };
        git_in(
            &root,
            &repo.config,
            "Someone",
            &["init", "-q", "--bare", "remote.git"],
        );
        repo.git("Someone", &["init", "-q"]);
        repo.git("Someone", &["remote", "add", "origin", "../remote.git"]);
        repo.commit("Someone", "Start", &[("readme.txt", "hello\n")]);
        repo.git("Someone", &["push", "-q", "origin", "main"]);
        repo
    }

    fn git(&self, who: &str, arguments: &[&str]) -> String {
        git_in(&self.dir, &self.config, who, arguments)
    }

    fn commit(&self, who: &str, message: &str, files: &[(&str, &str)]) -> String {
        for (name, text) in files {
            let path = self.dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        self.git(who, &["add", "-A"]);
        self.git(who, &["commit", "-q", "--allow-empty", "-m", message]);
        self.head()
    }

    fn head(&self) -> String {
        self.git("Someone", &["rev-parse", "HEAD"])
            .trim()
            .to_owned()
    }

    fn update(&self, local_ref: &str, local: &str, remote_ref: &str) -> Update {
        Update {
            local_ref: local_ref.to_owned(),
            local: local.to_owned(),
            remote_ref: remote_ref.to_owned(),
            remote: "0".repeat(40),
        }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap());
    }
}

/// A term in each place a push can carry one, each found and named
/// without the term.
#[test]
fn sys_a_push_is_searched_in_every_place_it_can_carry_a_term() {
    let repo = Repo::new("places");
    let base = repo.head();
    repo.git("Someone", &["switch", "-q", "-c", "work"]);
    // An added line, which the next commit takes out again: the first
    // commit still carries it, and stays reachable by its hash.
    let added = repo.commit("Someone", "Add notes", &[("notes.md", "one\nby Quentin\n")]);
    let removed = repo.commit("Someone", "Tidy", &[("notes.md", "one\n")]);
    // A path, an author, a committer and a message.
    let path = repo.commit("Someone", "Add a folder", &[("quentin/keep.txt", "x\n")]);
    let author = repo.commit("Example Person", "Clean", &[("b.txt", "b\n")]);
    let message = repo.commit("Someone", "As q.example@example.invalid asked", &[]);
    let clean = repo.commit("Someone", "Nothing here", &[("c.txt", "c\n")]);

    let short = |hash: &str| hash[..12].to_owned();
    let update = repo.update("refs/heads/work", &clean, "refs/heads/work");
    let (found, commits) = check(&repo.dir, "origin", &[update], &terms()).unwrap();
    assert_eq!(commits, 6, "the remote's first commit is not counted");
    let expected = [
        Found::Commit(Finding::Commit {
            commit: short(&message),
            field: "message",
        }),
        Found::Commit(Finding::Commit {
            commit: short(&author),
            field: "author's name",
        }),
        Found::Commit(Finding::Commit {
            commit: short(&author),
            field: "committer's name",
        }),
        Found::InCommit {
            commit: short(&added),
            finding: Finding::Line {
                path: Some("notes.md".to_owned()),
                line: 2,
            },
        },
        Found::InCommit {
            commit: short(&path),
            finding: Finding::Path { file: 1 },
        },
    ];
    for expected in &expected {
        assert!(found.contains(expected), "{expected:?} not in {found:?}");
    }
    assert_eq!(found.len(), expected.len(), "{found:?}");
    assert!(
        !found
            .iter()
            .any(|finding| finding.to_string().contains(&short(&removed)))
    );

    // The name of the branch the push writes, and an annotated tag's text.
    let tag_message = "Release for Quentin";
    repo.git("Someone", &["tag", "-a", "v1", "-m", tag_message, &base]);
    let tag = repo.git("Someone", &["rev-parse", "v1"]).trim().to_owned();
    let updates = [
        repo.update("refs/heads/main", &base, "refs/heads/quentin-work"),
        repo.update("refs/tags/v1", &tag, "refs/tags/v1"),
    ];
    let (found, commits) = check(&repo.dir, "origin", &updates, &terms()).unwrap();
    assert_eq!(commits, 0, "the remote has the base already");
    assert_eq!(
        found,
        [
            Found::Ref {
                line: 1,
                side: "remote"
            },
            Found::Tag {
                object: short(&tag)
            },
        ]
    );
}

#[test]
fn sys_a_clean_push_and_what_the_remote_has_find_nothing() {
    let repo = Repo::new("clean");
    // What the remote has already is not searched again, even when it
    // carries a term: it is out, and this push cannot take it back.
    repo.commit("Quentin", "Already out", &[("a.txt", "Quentin\n")]);
    repo.git("Someone", &["push", "-q", "origin", "main"]);
    let out = repo.head();
    let head = repo.commit("Someone", "Clean", &[("b.txt", "clean\n")]);
    let mut update = repo.update("refs/heads/main", &head, "refs/heads/main");
    update.remote = out;
    let (found, commits) = check(&repo.dir, "origin", &[update.clone()], &terms()).unwrap();
    assert_eq!((found, commits), (Vec::new(), 1));

    // A push to a URL, which names no remote, is held against every
    // remote's branches; a deletion sends nothing.
    let (found, commits) = check(&repo.dir, "../remote.git", &[update], &terms()).unwrap();
    assert_eq!((found, commits), (Vec::new(), 1));
    let deletion = repo.update("refs/heads/gone", &"0".repeat(40), "refs/heads/quentin");
    let (found, commits) = check(&repo.dir, "origin", &[deletion], &terms()).unwrap();
    assert_eq!((found, commits), (Vec::new(), 0));
}

#[test]
fn sys_a_failing_git_is_an_error_not_a_pass() {
    let repo = Repo::new("broken");
    let update = repo.update("refs/heads/main", &"e".repeat(40), "refs/heads/main");
    assert!(check(&repo.dir, "origin", &[update], &terms()).is_err());
}

/// The hook as git runs it: a real `git push`, the script from
/// `xtask/hooks/pre-push`, and in place of cargo a script that runs this
/// test binary's [`as_the_hook`], which runs the guard on what git gave the
/// hook.
#[cfg(unix)]
#[test]
fn sys_the_hook_refuses_a_push_with_a_term_and_lets_a_clean_one_go() {
    use std::os::unix::fs::PermissionsExt;

    let repo = Repo::new("hook");
    let root = repo.dir.parent().unwrap().to_owned();
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/pre-push");
    std::fs::copy(&hook, hooks.join("pre-push")).unwrap();
    repo.git(
        "Someone",
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );

    // cargo, in a CARGO_HOME of the test's own; it is not on PATH, which
    // holds git alone.
    let cargo_home = root.join("cargo-home");
    std::fs::create_dir_all(cargo_home.join("bin")).unwrap();
    let record = root.join("arguments");
    let fake = cargo_home.join("bin/cargo");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec '{}' --exact \
             owner_guard::local::tests::as_the_hook --ignored --quiet --nocapture --test-threads=1\n",
            record.display(),
            std::env::current_exe().unwrap().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let real_git = which("git");
    std::os::unix::fs::symlink(real_git, bin.join("git")).unwrap();

    let terms_file = root.join("owner-terms");
    std::fs::write(&terms_file, TERMS).unwrap();
    let push = |terms: Option<&Path>| {
        let mut command = git_command(&repo.dir, &repo.config, "Someone");
        command
            .args(["push", "origin", "HEAD:refs/heads/work"])
            .env("PATH", &bin)
            .env("HOME", &root)
            .env("CARGO_HOME", &cargo_home)
            .env_remove("CARGO")
            .env("XTASK_HOOK_RECORD", &record);
        match terms {
            Some(file) => command.env(TERMS_FILE, file),
            None => command.env_remove(TERMS_FILE),
        };
        let output = command.output().unwrap();
        let printed = String::from_utf8_lossy(&output.stderr).into_owned();
        (output.status.success(), printed)
    };

    repo.commit("Someone", "By Quentin", &[("a.txt", "a\n")]);
    let (pushed, printed) = push(Some(&terms_file));
    assert!(!pushed, "a push with a term went: {printed}");
    assert!(
        printed.contains("its message matches a protected term"),
        "{printed}"
    );
    assert!(!terms().matches(&printed), "a term was printed: {printed}");
    let arguments = std::fs::read_to_string(&record).unwrap();
    assert_eq!(
        arguments.lines().take(3).collect::<Vec<_>>(),
        ["xtask", "owner-guard", "--pre-push"]
    );
    assert_eq!(arguments.lines().nth(3), Some("origin"));

    // Without the file (none named, none under this home folder): a
    // warning, and the push goes.
    let (pushed, printed) = push(None);
    assert!(pushed, "{printed}");
    assert!(printed.contains("nothing was checked"), "{printed}");

    // With it, a clean commit on top of what is out goes.
    repo.commit("Someone", "Clean", &[("b.txt", "b\n")]);
    let (pushed, printed) = push(Some(&terms_file));
    assert!(pushed, "{printed}");
    assert!(
        printed.contains("no protected term in the 1 commit(s)"),
        "{printed}"
    );
}

/// The guard as the hook's cargo runs it, in [`sys_the_hook_refuses_a_push_with_a_term_and_lets_a_clean_one_go`]:
/// the arguments are in the file the fake cargo wrote, the updates on
/// standard input. Outside that test it does nothing.
#[cfg(unix)]
#[test]
#[ignore = "run by the hook test, as the hook's cargo"]
fn as_the_hook() {
    let Some(record) = std::env::var_os("XTASK_HOOK_RECORD") else {
        return;
    };
    let arguments: Vec<String> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .skip(2)
        .map(str::to_owned)
        .collect();
    if let Err(message) = super::run(&arguments) {
        panic!("{message}");
    }
}

#[cfg(unix)]
fn which(program: &str) -> PathBuf {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{program} is not on PATH"))
}
