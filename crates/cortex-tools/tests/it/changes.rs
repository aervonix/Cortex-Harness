//! Which files a command changed, with git and without it
//! (`docs/architecture/07-sessions-and-context.md`, "Changes made by shell
//! commands").
//!
//! The tests with git start `git`, so they are system tests. Each makes a
//! repository of its own, with an identity and a configuration of its own:
//! nothing of the machine's git set-up is read.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use cortex_tools::shell::ChangeDetector;
use cortex_tools::shell::changes::Method;
use cortex_tools::{Concurrency, NoOutput, ShellKind, Tool, execute};
use serde_json::json;

use crate::bench::{Bench, default_shell};
use crate::{Workspace, block_on, remove_all};

/// Runs git in `directory`, with nothing of the machine's configuration.
pub(crate) fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args([
            "-c",
            "user.name=Cortex Test",
            "-c",
            "user.email=test@cortex-harness.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.autocrlf=false",
        ])
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("no-such-git-config"))
        .output()
        .expect("git is installed where these tests run");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A repository at the workspace root, with one commit of `files`.
pub(crate) fn repository(ws: &Workspace, files: &[(&str, &str)]) -> PathBuf {
    let root = ws.root();
    git(&root, &["init", "--quiet"]);
    for (path, content) in files {
        ws.file(path, content);
    }
    git(&root, &["add", "--all"]);
    git(&root, &["commit", "--quiet", "--message", "first"]);
    root
}

fn names(list: &[String]) -> Vec<&str> {
    list.iter().map(String::as_str).collect()
}

#[test]
fn sys_git_says_what_a_command_changed() {
    let ws = Workspace::new("changes_git");
    let root = repository(
        &ws,
        &[
            ("changed.txt", "as committed\n"),
            ("deleted.txt", "as committed\n"),
            ("changed before.txt", "as committed\n"),
            ("untouched.txt", "as committed\n"),
            (".gitignore", "build/\n"),
        ],
    );
    // What the workspace looks like before the command: one file already
    // changed, one already there and unknown to git.
    ws.file(
        "changed before.txt",
        "changed by hand, before the command\n",
    );
    ws.file("untracked before.txt", "here all along\n");
    let head = git(&root, &["rev-parse", "HEAD"]);

    let detector = ChangeDetector::new(&root).trusting_the_repository(true);
    let before = detector.before();
    // The command.
    ws.file("changed.txt", "changed by the command, and longer\n");
    std::fs::remove_file(root.join("deleted.txt")).expect("delete a file");
    ws.file(
        "changed before.txt",
        "changed again, by the command this time\n",
    );
    ws.file("sub dir/created.txt", "new\n");
    ws.file("build/output.o", "ignored\n");
    let changes = detector.after(&before);

    assert_eq!(changes.method, Method::Git);
    assert!(changes.complete);
    assert!(!changes.head_moved);
    assert_eq!(
        names(&changes.modified),
        ["changed before.txt", "changed.txt"]
    );
    assert_eq!(names(&changes.created), ["sub dir/created.txt"]);
    assert_eq!(names(&changes.deleted), ["deleted.txt"]);
    // What the command replaced can be had back from the commit for the
    // two files that were as committed, and not for the one that was not.
    assert_eq!(names(&changes.restorable), ["changed.txt", "deleted.txt"]);
    assert_eq!(changes.base_commit.as_deref(), Some(head.as_str()));

    // A command that changes nothing changes nothing.
    let before = detector.before();
    let changes = detector.after(&before);
    assert!(changes.is_empty(), "{changes:?}");
    assert!(changes.restorable.is_empty());
    assert!(changes.complete);
}

#[test]
fn sys_git_follows_a_commit_and_a_checkout() {
    let ws = Workspace::new("changes_git_head");
    let root = repository(&ws, &[("kept.txt", "one\n"), ("swapped.txt", "on main\n")]);
    let main = git(&root, &["rev-parse", "HEAD"]);
    // Another branch, where one file differs, one is new and one is gone.
    git(&root, &["checkout", "--quiet", "-b", "other"]);
    ws.file("swapped.txt", "on the other branch, and longer\n");
    ws.file("only on other.txt", "new\n");
    std::fs::remove_file(root.join("kept.txt")).expect("delete a file");
    git(&root, &["add", "--all"]);
    git(&root, &["commit", "--quiet", "--message", "second"]);
    git(&root, &["checkout", "--quiet", "main"]);

    let detector = ChangeDetector::new(&root).trusting_the_repository(true);

    // A commit of a file as it is changes no file, and moves the head.
    ws.file("kept.txt", "one, and changed by hand\n");
    let before = detector.before();
    git(&root, &["commit", "--quiet", "--all", "--message", "third"]);
    let changes = detector.after(&before);
    assert!(changes.is_empty(), "{changes:?}");
    assert!(changes.head_moved);
    assert!(changes.complete);
    assert_eq!(changes.base_commit.as_deref(), Some(main.as_str()));
    let third = git(&root, &["rev-parse", "HEAD"]);

    // A checkout changes files that git calls clean before and after. The
    // two commits say which.
    let before = detector.before();
    git(&root, &["checkout", "--quiet", "other"]);
    let changes = detector.after(&before);
    assert!(changes.head_moved);
    assert_eq!(names(&changes.modified), ["swapped.txt"]);
    assert_eq!(names(&changes.created), ["only on other.txt"]);
    assert_eq!(names(&changes.deleted), ["kept.txt"]);
    assert_eq!(names(&changes.restorable), ["kept.txt", "swapped.txt"]);
    assert_eq!(changes.base_commit.as_deref(), Some(third.as_str()));
}

#[test]
fn sys_a_workspace_below_the_repository_sees_its_own_part() {
    let ws = Workspace::new("changes_git_below");
    let root = repository(
        &ws,
        &[
            ("outside.txt", "one\n"),
            ("deep/work space/inside.txt", "one\n"),
        ],
    );
    let below = root.join("deep").join("work space");
    let detector = ChangeDetector::new(&below).trusting_the_repository(true);
    let before = detector.before();
    ws.file("outside.txt", "changed, outside the workspace\n");
    ws.file(
        "deep/work space/inside.txt",
        "changed, inside the workspace\n",
    );
    ws.file("deep/work space/more/new.txt", "new\n");
    let changes = detector.after(&before);
    // Relative to the workspace, and nothing from above it.
    assert_eq!(names(&changes.modified), ["inside.txt"]);
    assert_eq!(names(&changes.created), ["more/new.txt"]);
    assert_eq!(names(&changes.restorable), ["inside.txt"]);
    assert_eq!(changes.method, Method::Git);
}

/// A repository can name a program for git to run whenever it is asked
/// what has changed. Nobody approved that program, and the harness's own
/// questions to git do not run it.
#[cfg(unix)]
#[test]
fn sys_asking_git_runs_no_program_the_repository_names() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::new("changes_git_fsmonitor");
    let root = repository(&ws, &[("a.txt", "one\n")]);
    let marker = ws.dir.join("the-program-ran");
    let program = ws.dir.join("monitor.sh");
    std::fs::write(
        &program,
        format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
    )
    .expect("write the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    git(
        &root,
        &["config", "core.fsmonitor", program.to_str().expect("text")],
    );
    // An ordinary `git status` does run it: the test would mean nothing
    // if the setting did nothing.
    git(&root, &["status", "--porcelain"]);
    assert!(marker.exists(), "git did not run the program at all");
    std::fs::remove_file(&marker).expect("remove the marker");

    let detector = ChangeDetector::new(&root).trusting_the_repository(true);
    let before = detector.before();
    ws.file("a.txt", "changed, and longer\n");
    let changes = detector.after(&before);
    assert_eq!(names(&changes.modified), ["a.txt"]);
    assert!(
        !marker.exists(),
        "the harness's question ran the repository's program"
    );
}

/// A repository can also name a filter, which git runs on a file to see
/// whether it has changed, and nothing turns every filter off. So a
/// project that is not trusted is not asked through git at all.
#[cfg(unix)]
#[test]
fn sys_a_project_that_is_not_trusted_is_not_asked_through_git() {
    let ws = Workspace::new("changes_git_untrusted");
    let root = repository(
        &ws,
        &[
            ("a.txt", "one\n"),
            (".gitattributes", "*.txt filter=planted\n"),
        ],
    );
    let marker = ws.dir.join("the-filter-ran");
    git(
        &root,
        &[
            "config",
            "filter.planted.clean",
            &format!("sh -c 'echo ran > \"{}\"; cat'", marker.display()),
        ],
    );

    // Not trusted: the files themselves are looked at, and git is not run.
    let detector = ChangeDetector::new(&root);
    let before = detector.before();
    ws.file("a.txt", "changed by the command, and longer\n");
    let changes = detector.after(&before);
    assert_eq!(changes.method, Method::ModificationTimes);
    // The files of the repository are a moment old too, and may be inside
    // the slack this way of looking allows: what matters is that the
    // changed file is found.
    assert!(
        changes.modified.contains(&"a.txt".to_owned()),
        "{changes:?}"
    );
    assert!(changes.restorable.is_empty());
    assert!(!marker.exists(), "an untrusted repository's filter was run");

    // Trusted, git is asked, and git does run the filter: the user has
    // accepted this repository's configuration. Git runs it when it has to
    // read a file to know whether it changed, which a file of the same
    // size as the committed one makes it do.
    ws.file("a.txt", "two\n");
    let trusting = ChangeDetector::new(&root).trusting_the_repository(true);
    let before = trusting.before();
    let changes = trusting.after(&before);
    assert_eq!(changes.method, Method::Git);
    assert!(
        marker.exists(),
        "the filter is live, so the test above means something"
    );
}

#[test]
fn sys_git_that_does_not_answer_in_time_is_not_waited_for() {
    let ws = Workspace::new("changes_git_slow");
    let root = repository(&ws, &[("a.txt", "one\n")]);
    // No time at all: git cannot answer, and nothing else is tried.
    let detector = ChangeDetector::new(&root)
        .trusting_the_repository(true)
        .with_cap(Duration::ZERO);
    let before = detector.before();
    ws.file("a.txt", "changed\n");
    let changes = detector.after(&before);
    assert_eq!(changes.method, Method::None);
    assert!(!changes.complete);
    assert!(changes.is_empty());
}

/// A directory of the test's own that is in no repository: these tests'
/// usual place is inside this one.
struct Outside(PathBuf);

impl Outside {
    fn new(test: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("cortex-tools-it-{test}-{}", std::process::id()));
        remove_all(&directory);
        std::fs::create_dir_all(&directory).expect("create a directory");
        for above in directory.ancestors() {
            assert!(
                !above.join(".git").exists(),
                "the temporary directory is inside a repository: {}",
                above.display()
            );
        }
        // The real location: the temporary directory may be a link.
        Self(std::fs::canonicalize(&directory).expect("the directory exists"))
    }

    fn file(&self, relative: &str, content: &str, age: Duration) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create a directory");
        std::fs::write(&path, content).expect("write a file");
        if !age.is_zero() {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("open the file")
                .set_modified(SystemTime::now() - age)
                .expect("set the modification time");
        }
    }
}

impl Drop for Outside {
    fn drop(&mut self) {
        remove_all(&self.0);
    }
}

#[test]
fn without_git_changes_are_found_by_modification_time() {
    let outside = Outside::new("changes_mtime");
    let hour = Duration::from_secs(3600);
    outside.file("old.txt", "long here\n", hour);
    outside.file("to change.txt", "long here\n", hour);
    outside.file(".ignore", "build/\n", hour);

    let detector = ChangeDetector::new(&outside.0);
    let before = detector.before();
    // The command.
    outside.file("to change.txt", "changed by the command\n", Duration::ZERO);
    outside.file("sub/created.txt", "new\n", Duration::ZERO);
    outside.file(
        "build/output.o",
        "left out by the ignore file\n",
        Duration::ZERO,
    );
    let changes = detector.after(&before);

    assert_eq!(changes.method, Method::ModificationTimes);
    assert!(changes.complete);
    // A new file cannot be told from a changed one this way, a deleted one
    // cannot be seen, and nothing can be had back.
    assert_eq!(
        names(&changes.modified),
        ["sub/created.txt", "to change.txt"]
    );
    assert!(changes.created.is_empty() && changes.deleted.is_empty());
    assert!(changes.restorable.is_empty());
    assert_eq!(changes.base_commit, None);
    assert_eq!(
        changes.to_json()["method"],
        json!("mtime"),
        "{}",
        changes.to_json()
    );
}

#[test]
fn a_look_that_runs_out_of_time_says_it_is_not_complete() {
    let outside = Outside::new("changes_mtime_cap");
    let write_all = || {
        for number in 0..200 {
            outside.file(&format!("dir/file {number}.txt"), "x\n", Duration::ZERO);
        }
    };
    // The files are written after the first look, as a command's are,
    // however long writing two hundred of them takes on this machine.
    let detector = ChangeDetector::new(&outside.0);
    let before = detector.before();
    write_all();
    let all = detector.after(&before);
    assert!(all.complete);
    assert_eq!(all.modified.len(), 200);

    // With no time, the look stops at its first check of the clock.
    let hurried = ChangeDetector::new(&outside.0).with_cap(Duration::ZERO);
    let before = hurried.before();
    write_all();
    let some = hurried.after(&before);
    assert!(!some.complete);
    assert!(some.modified.len() < 200, "{}", some.modified.len());
    assert_eq!(some.method, Method::ModificationTimes);
}

/// The whole way: a command run by the tool, and the changes in the
/// result's payload, where the session finds them.
#[test]
fn sys_the_tool_reports_what_its_command_changed() {
    let ws = Workspace::new("changes_through_the_tool");
    let root = repository(&ws, &[("changed.txt", "one\n"), ("deleted.txt", "one\n")]);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let bench = Bench::in_workspace(ws, default_shell(), |config| {
        config.detect_changes = true;
        config.repository_trusted = true;
    });
    let command = match bench.shell().kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            "Add-Content changed.txt 'and more'; Remove-Item deleted.txt; \
             Set-Content created.txt 'new'"
        }
        ShellKind::Cmd => "echo and more>> changed.txt& del deleted.txt& echo new> created.txt",
        _ => "echo 'and more' >> changed.txt; rm deleted.txt; echo new > created.txt",
    };
    let ran = bench.command(command);
    assert_eq!(
        ran.payload("changes"),
        json!({
            "method": "git",
            "modified": ["changed.txt"],
            "created": ["created.txt"],
            "deleted": ["deleted.txt"],
            "restorable": ["changed.txt", "deleted.txt"],
            "base_commit": head,
            "head_moved": false,
            "complete": true,
        }),
        "{}",
        ran.text()
    );
    // The model is not told: it knows what it ran.
    assert!(!ran.text().contains("changed.txt"), "{}", ran.text());

    // A call the permission engine marked read-only is not looked at.
    let cx = bench.ws.cx();
    let mut plan = bench
        .tool
        .plan(&json!({ "command": "echo nothing changes" }), &cx.plan_cx())
        .expect("a plan");
    plan.concurrency = Concurrency::ReadOnly;
    let result = block_on(execute(&bench.tool, plan, &cx, &mut NoOutput));
    let payload = result.payload().expect("a payload");
    assert_eq!(payload["changes"], json!(null));
    assert_eq!(payload["exit_code"], json!(0));
}

/// Where every git the look starts runs: as the confinement it was given
/// says (`cortex_base::git::Confinement`), which in a session that confines
/// commands is the sandbox. This one records what it is asked to start and
/// starts it as it is, or refuses.
#[derive(Debug, Default)]
struct Recording {
    asked: std::sync::Mutex<Vec<String>>,
    refuse: bool,
}

impl cortex_base::git::Confinement for Recording {
    fn confines(&self) -> bool {
        true
    }

    fn start(
        &self,
        program: &Path,
        arguments: Vec<std::ffi::OsString>,
    ) -> Result<cortex_base::git::Started, String> {
        if self.refuse {
            return Err("refused".to_owned());
        }
        // The first argument after the hardening is the subcommand.
        let subcommand = arguments[cortex_base::GIT_HARDENING.len()]
            .to_string_lossy()
            .into_owned();
        self.asked.lock().unwrap().push(subcommand);
        Ok(cortex_base::git::Started {
            program: program.to_owned(),
            arguments,
            stdin: None,
        })
    }
}

#[test]
fn sys_every_git_the_look_starts_goes_through_its_confinement() {
    let ws = Workspace::new("changes_confined");
    let root = repository(&ws, &[("a.txt", "one\n")]);
    let recording = std::sync::Arc::new(Recording::default());
    let detector = ChangeDetector::new(&root)
        .trusting_the_repository(true)
        .with_git(recording.clone());
    let before = detector.before();
    // A command that commits: the look after it compares the two commits.
    ws.file("a.txt", "two\n");
    git(
        &root,
        &["commit", "--quiet", "--all", "--message", "second"],
    );
    let changes = detector.after(&before);
    assert!(changes.complete, "{changes:?}");
    assert!(changes.head_moved);
    assert_eq!(names(&changes.modified), ["a.txt"]);
    assert_eq!(
        *recording.asked.lock().unwrap(),
        ["status", "status", "diff"]
    );

    // A confinement that refuses: git is not started, and the look says
    // that it is incomplete, and why.
    let refusing = std::sync::Arc::new(Recording {
        refuse: true,
        ..Recording::default()
    });
    let detector = ChangeDetector::new(&root)
        .trusting_the_repository(true)
        .with_git(refusing);
    let before = detector.before();
    ws.file("a.txt", "three\n");
    let changes = detector.after(&before);
    assert!(!changes.complete);
    assert_eq!(
        changes.incomplete,
        Some(cortex_tools::shell::Incomplete::GitNotConfined)
    );
    assert!(changes.is_empty(), "{changes:?}");
}
