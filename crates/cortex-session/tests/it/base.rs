//! The workspace's starting state.

use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cortex_protocol::ids::SessionId;
use cortex_session::base::{GIT_HARDENING, GitOutput, OmitReason, capture};
use cortex_session::entry::{Body, Kind};
use cortex_session::{BaseCapture, BaseLimits, Git, Log, SystemGit};
use cortex_testkit::Gate;

use crate::{World, assistant, in_ci, user};

/// A git that answers from a script, records what it was asked, and can be
/// held back until a gate opens.
#[derive(Debug, Default)]
struct ScriptedGit {
    /// Opened before the first answer, when set.
    hold: Option<Arc<Gate>>,
    /// Answers by the first arguments (after the hardening flags).
    answers: Vec<(Vec<&'static str>, Vec<u8>)>,
    asked: Mutex<Vec<Vec<String>>>,
}

impl ScriptedGit {
    fn repository(commit: &'static str) -> Self {
        Self {
            answers: vec![
                (vec!["rev-parse", "--show-toplevel"], b"TOP\n".to_vec()),
                (
                    vec!["rev-parse", "--verify"],
                    format!("{commit}\n").into_bytes(),
                ),
                (vec!["symbolic-ref"], b"main\n".to_vec()),
                (
                    vec!["diff", "--name-only"],
                    b"src/lib.rs\0.env\0certs/server.pem\0".to_vec(),
                ),
                (
                    vec!["diff", "--no-ext-diff"],
                    b"diff --git a/src/lib.rs b/src/lib.rs\n".to_vec(),
                ),
                (
                    vec!["ls-files"],
                    b"notes.txt\0.env.local\0id_ed25519\0".to_vec(),
                ),
            ],
            ..Self::default()
        }
    }

    fn asked(&self) -> Vec<Vec<String>> {
        self.asked.lock().unwrap().clone()
    }
}

impl Git for ScriptedGit {
    fn run(&self, _dir: &Path, args: &[&str], _limit: usize) -> io::Result<GitOutput> {
        if let Some(gate) = &self.hold {
            assert!(
                gate.wait(Duration::from_secs(5)),
                "the test never let git answer"
            );
        }
        self.asked
            .lock()
            .unwrap()
            .push(args.iter().map(ToString::to_string).collect());
        let answer = self
            .answers
            .iter()
            .find(|(prefix, _)| args.starts_with(prefix));
        Ok(match answer {
            Some((_, stdout)) => GitOutput {
                success: true,
                stdout: stdout.clone(),
                truncated: false,
            },
            None => GitOutput {
                success: false,
                stdout: Vec::new(),
                truncated: false,
            },
        })
    }
}

#[test]
fn the_base_state_does_not_delay_the_first_prompt() {
    let world = World::new("base_background");
    let gate = Arc::new(Gate::new());
    let git = Arc::new(ScriptedGit {
        hold: Some(Arc::clone(&gate)),
        ..ScriptedGit::repository("abc123")
    });
    let mut session = world.create("s_1");
    let capture = BaseCapture::start(
        world.root.clone(),
        false,
        session.store(),
        git.clone(),
        Arc::new(world.clock.clone()),
        BaseLimits::default(),
    )
    .unwrap();
    // git has not answered, and the conversation goes on.
    let prompt = session.append(None, user("first prompt")).unwrap();
    session
        .append(None, assistant("i_2", "answer", &[]))
        .unwrap();
    assert!(git.asked().is_empty());
    gate.open();
    let base = capture
        .wait_for(Duration::from_secs(5))
        .expect("the base state arrives");
    let recorded = session.append(None, Body::Base(base.clone())).unwrap();
    assert!(session.is_record(&recorded));
    assert!(recorded > prompt, "recorded after the first prompt");
    assert_eq!(session.base().unwrap(), Some(base));
}

#[test]
fn an_untrusted_project_gets_its_commit_and_branch_and_nothing_that_runs_its_configuration() {
    let world = World::new("base_untrusted");
    let session = world.create("s_1");
    let git = ScriptedGit::repository("abc123");
    let base = capture(
        &world.root,
        false,
        &session.store(),
        &git,
        &world.clock,
        &BaseLimits::default(),
    );
    assert!(!base.complete);
    assert!(!base.trusted);
    assert!(base.note.as_deref().unwrap().contains("not trusted"));
    let state = base.git.unwrap();
    assert_eq!(state.commit.as_deref(), Some("abc123"));
    assert_eq!(state.branch.as_deref(), Some("main"));
    assert!(state.patch.is_none() && state.untracked.is_empty());
    let commands: Vec<String> = git.asked().iter().map(|args| args[0].clone()).collect();
    assert_eq!(commands, ["rev-parse", "rev-parse", "symbolic-ref"]);
}

#[test]
fn likely_secrets_are_never_kept() {
    let world = World::new("base_secrets");
    let session = world.create("s_1");
    let top = world.workspace.to_string_lossy().into_owned();
    let mut git = ScriptedGit::repository("abc123");
    git.answers[0].1 = format!("{top}\n").into_bytes();
    world.write("notes.txt", b"untracked notes");
    world.write(".env.local", b"KEY=hunter2");
    world.write("id_ed25519", b"PRIVATE");
    git.answers[5].1 = b"notes.txt\0.env.local\0id_ed25519\0".to_vec();
    let base = capture(
        &world.root,
        true,
        &session.store(),
        &git,
        &world.clock,
        &BaseLimits::default(),
    );
    let state = base.git.clone().unwrap();
    let secret: Vec<&str> = base
        .omitted
        .iter()
        .filter(|omitted| omitted.reason == OmitReason::Secret)
        .map(|omitted| omitted.path.as_str())
        .collect();
    assert_eq!(
        secret,
        [".env", "certs/server.pem", ".env.local", "id_ed25519"]
    );
    assert!(base.complete, "{base:?}");
    assert_eq!(state.untracked.len(), 1);
    assert_eq!(state.untracked[0].path, "notes.txt");
    assert!(state.patch.is_some());
    // The tracked secrets are left out of the patch by name.
    let diff = git
        .asked()
        .into_iter()
        .find(|args| args[0] == "diff" && args[1] == "--no-ext-diff")
        .unwrap();
    assert!(
        diff.contains(&":(exclude,literal).env".to_owned()),
        "{diff:?}"
    );
    assert!(
        diff.contains(&":(exclude,literal)certs/server.pem".to_owned()),
        "{diff:?}"
    );
    for flag in ["--binary", "--no-textconv", "--full-index"] {
        assert!(diff.iter().any(|arg| arg == flag), "{flag}");
    }
    assert!(
        !session
            .store()
            .has_blob(&cortex_base::ContentHash::of(b"KEY=hunter2"))
    );
}

#[test]
fn a_workspace_outside_git_is_listed_with_hashes() {
    let world = World::new("base_listing");
    world.write("src/main.rs", b"fn main() {}");
    world.write("README.md", b"# hello");
    world.write(".env", b"SECRET=1");
    let session = world.create("s_1");
    let git = ScriptedGit::default(); // every command fails: not a repository
    let base = capture(
        &world.root,
        false,
        &session.store(),
        &git,
        &world.clock,
        &BaseLimits::default(),
    );
    assert!(base.complete, "{base:?}");
    assert!(base.git.is_none());
    let files = base.files.unwrap();
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["README.md", "src/main.rs"]);
    assert_eq!(
        files[1].hash,
        Some(cortex_base::ContentHash::of(b"fn main() {}"))
    );
    assert_eq!(base.omitted[0].path, ".env");

    let limits = BaseLimits {
        listed_files: 1,
        ..BaseLimits::default()
    };
    let base = capture(
        &world.root,
        false,
        &session.store(),
        &git,
        &world.clock,
        &limits,
    );
    assert!(!base.complete);
    assert!(
        base.omitted
            .iter()
            .any(|omitted| omitted.reason == OmitReason::TooMany)
    );
}

/// Runs a real git command in `dir`, for setting up a repository.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn has_git() -> bool {
    let found = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(found || !in_ci(), "git is needed in CI");
    found
}

#[test]
fn sys_a_git_workspace_records_what_replay_needs() {
    if !has_git() {
        return;
    }
    let world = World::new("sys_base_git");
    let dir = world.workspace.clone();
    git(&dir, &["init", "--quiet", "--initial-branch=work"]);
    git(&dir, &["config", "core.autocrlf", "false"]);
    world.write("src/lib.rs", b"pub fn one() {}\n");
    world.write("data.bin", &[0, 1, 2, 3]);
    world.write(".env", b"TOKEN=tracked");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "start"]);
    let commit = git(&dir, &["rev-parse", "HEAD"]);
    world.write("src/lib.rs", b"pub fn one() {}\npub fn two() {}\n");
    world.write("data.bin", &[9, 9]);
    world.write(".env", b"TOKEN=changed");
    world.write("notes.txt", b"not tracked");
    world.write(".env.local", b"TOKEN=untracked");
    world.write(".gitignore", b"target/\n");
    world.write("target/out", b"ignored");

    let session = world.create("s_1");
    let store = session.store();
    let base = capture(
        &world.root,
        true,
        &store,
        &SystemGit,
        &world.clock,
        &BaseLimits::default(),
    );
    assert!(base.complete, "{base:?}");
    let state = base.git.clone().unwrap();
    assert_eq!(state.commit.as_deref(), Some(commit.as_str()));
    assert_eq!(state.branch.as_deref(), Some("work"));
    assert_eq!(state.root, "");
    let mut untracked: Vec<&str> = state
        .untracked
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    untracked.sort_unstable();
    assert_eq!(untracked, [".gitignore", "notes.txt"]);
    let mut omitted: Vec<&str> = base
        .omitted
        .iter()
        .map(|omitted| omitted.path.as_str())
        .collect();
    omitted.sort_unstable();
    assert_eq!(omitted, [".env", ".env.local"]);

    // The patch replays: a fresh checkout of the commit, with the patch
    // applied, has the tracked files as they were (but the secret).
    let mut patch = Vec::new();
    assert!(cortex_session::base::write_patch(&store, &state, &mut patch).unwrap());
    let replay = world.scratch.dir.join("replay");
    git(
        &world.scratch.dir,
        &[
            "clone",
            "--quiet",
            "-c",
            "core.autocrlf=false",
            dir.to_str().unwrap(),
            "replay",
        ],
    );
    git(&replay, &["checkout", "--quiet", &commit]);
    let patch_file = world.scratch.dir.join("base.patch");
    std::fs::write(&patch_file, &patch).unwrap();
    git(
        &replay,
        &["apply", "--binary", patch_file.to_str().unwrap()],
    );
    assert_eq!(
        std::fs::read(replay.join("src/lib.rs")).unwrap(),
        b"pub fn one() {}\npub fn two() {}\n"
    );
    assert_eq!(std::fs::read(replay.join("data.bin")).unwrap(), [9, 9]);
    assert_eq!(
        std::fs::read(replay.join(".env")).unwrap(),
        b"TOKEN=tracked"
    );
    let notes = state
        .untracked
        .iter()
        .find(|file| file.path == "notes.txt")
        .unwrap();
    assert_eq!(
        store.get_blob(&notes.hash).unwrap().unwrap(),
        b"not tracked"
    );

    // Nothing in the repository was changed by taking it.
    let status = git(&dir, &["status", "--porcelain"]);
    assert!(status.contains(" M src/lib.rs"), "{status}");
    assert!(GIT_HARDENING.contains(&"core.fsmonitor=false"));
}

#[test]
fn sys_the_base_state_is_recorded_in_the_log_from_a_real_capture() {
    if !has_git() {
        return;
    }
    let world = World::new("sys_base_log");
    world.write("plain.txt", b"no repository here");
    let mut session = world.create("s_1");
    let capture = BaseCapture::start(
        world.root.clone(),
        true,
        session.store(),
        Arc::new(SystemGit),
        Arc::new(world.clock.clone()),
        BaseLimits::default(),
    )
    .unwrap();
    session.append(None, user("go")).unwrap();
    let base = capture.wait().unwrap();
    session.append(None, Body::Base(base)).unwrap();
    drop(session);
    let (log, _) = Log::open(&world.project.session(&SessionId::new("s_1")).unwrap()).unwrap();
    let base = log.base().unwrap().unwrap();
    // The scratch directory is inside the repository's own working tree
    // when the tests run from a checkout, so either form is right; what
    // matters is that a state was taken and recorded.
    assert!(base.git.is_some() || base.files.is_some());
    assert_eq!(log.records(&Kind::Base).unwrap().len(), 1);
}
