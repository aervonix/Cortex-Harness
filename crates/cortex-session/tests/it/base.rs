//! The workspace's starting state.

use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cortex_base::AbsPath;
use cortex_protocol::ids::SessionId;
use cortex_session::base::{GIT_HARDENING, GitOutput, OmitReason, capture};
use cortex_session::entry::{Body, Kind};
use cortex_session::{BaseCapture, BaseLimits, Git, Log, SessionError, SystemGit};
use cortex_testkit::Gate;

use crate::{World, assistant, in_ci, user};

/// The gate these tests give a capture, standing in for the permission
/// engine's read rules: files that look like secrets are not kept. The list
/// is the test's. `cortex-session` has none of its own.
fn no_secrets(path: &AbsPath) -> bool {
    let name = path.file_name().unwrap_or_default().to_ascii_lowercase();
    !(name.starts_with(".env")
        || name.ends_with(".pem")
        || name == "id_ed25519"
        || name.contains("secret"))
}

/// A git that answers from a script, records what it was asked, and can be
/// held back until a gate opens.
#[derive(Debug, Default)]
struct ScriptedGit {
    /// Opened before the first answer, when set.
    hold: Option<Arc<Gate>>,
    /// Answers by the first arguments (after the hardening flags).
    answers: Vec<(Vec<&'static str>, Vec<u8>)>,
    /// Commands, by their first arguments, whose output is over the limit.
    too_long: Vec<Vec<&'static str>>,
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
        if self.too_long.iter().any(|prefix| args.starts_with(prefix)) {
            return Ok(GitOutput {
                success: false,
                stdout: Vec::new(),
                truncated: true,
            });
        }
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
        Arc::new(no_secrets),
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
        &no_secrets,
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

/// The store keeps no list of secret names of its own: what is kept is
/// what the gate lets through, asked about each file's absolute path.
#[test]
fn the_gate_alone_decides_what_is_kept() {
    let world = World::new("base_gate_alone");
    let session = world.create("s_1");
    let top = world.workspace.to_string_lossy().into_owned();
    let mut git = ScriptedGit::repository("abc123");
    git.answers[0].1 = format!("{top}\n").into_bytes();
    git.answers[3].1 = b"src/id_map.rs\0".to_vec();
    world.write("src/id_map.rs", b"ordinary source");
    world.write(".env.local", b"KEY=hunter2");
    world.write("notes.txt", b"untracked notes");
    git.answers[5].1 = b"notes.txt\0.env.local\0src/id_map.rs\0".to_vec();

    // A gate that lets everything through: nothing is held back by name.
    let asked = Mutex::new(Vec::new());
    let open = |path: &AbsPath| {
        asked.lock().unwrap().push(path.clone());
        true
    };
    let base = capture(
        &world.root,
        true,
        &session.store(),
        &git,
        &world.clock,
        &BaseLimits::default(),
        &open,
    );
    assert!(base.omitted.is_empty(), "{:?}", base.omitted);
    let kept: Vec<String> = base
        .git
        .unwrap()
        .untracked
        .into_iter()
        .map(|file| file.path)
        .collect();
    assert_eq!(kept, ["notes.txt", ".env.local", "src/id_map.rs"]);
    // Each file was asked about by its absolute path under git's top.
    let asked = asked.into_inner().unwrap();
    assert!(
        asked.contains(&world.root.join("src/id_map.rs").unwrap()),
        "{asked:?}"
    );
    assert!(asked.contains(&world.root.join(".env.local").unwrap()));

    // A gate that refuses one ordinary-looking file: that file is left out,
    // of the store and of the patch.
    let session = world.create("s_2");
    let git = {
        let mut git = ScriptedGit::repository("abc123");
        git.answers[0].1 = format!("{top}\n").into_bytes();
        git.answers[3].1 = b"notes.txt\0src/lib.rs\0".to_vec();
        git.answers[5].1 = b"notes.txt\0.env.local\0".to_vec();
        git
    };
    let no_notes = |path: &AbsPath| path.file_name() != Some("notes.txt");
    let base = capture(
        &world.root,
        true,
        &session.store(),
        &git,
        &world.clock,
        &BaseLimits::default(),
        &no_notes,
    );
    let protected: Vec<&str> = base
        .omitted
        .iter()
        .filter(|omitted| omitted.reason == OmitReason::Protected)
        .map(|omitted| omitted.path.as_str())
        .collect();
    assert_eq!(
        protected,
        ["notes.txt", "notes.txt"],
        "tracked, then untracked"
    );
    assert!(
        !session
            .store()
            .has_blob(&cortex_base::ContentHash::of(b"untracked notes"))
    );
    let diff = git
        .asked()
        .into_iter()
        .find(|args| args[0] == "diff" && args[1] == "--no-ext-diff")
        .unwrap();
    assert!(
        diff.contains(&":(exclude,literal)notes.txt".to_owned()),
        "{diff:?}"
    );
}

#[test]
fn what_the_gate_refuses_is_never_kept() {
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
        &no_secrets,
    );
    let state = base.git.clone().unwrap();
    let secret: Vec<&str> = base
        .omitted
        .iter()
        .filter(|omitted| omitted.reason == OmitReason::Protected)
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
        &no_secrets,
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
        &no_secrets,
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
        &no_secrets,
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
        Arc::new(no_secrets),
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

/// A scripted repository whose top is the test's workspace, with no
/// changes to tracked files and these untracked ones.
fn untracked_repository(world: &World, untracked: &[&str]) -> ScriptedGit {
    let mut git = ScriptedGit::repository("abc123");
    git.answers[0].1 = format!("{}\n", world.workspace.to_string_lossy()).into_bytes();
    git.answers[3].1 = Vec::new();
    git.answers[5].1 = untracked
        .iter()
        .flat_map(|name| [name.as_bytes(), b"\0"].concat())
        .collect();
    git
}

#[test]
fn untracked_files_past_a_limit_on_the_whole_are_recorded_once() {
    let world = World::new("base_overflow");
    let names = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
    for name in names {
        world.write(name, b"four");
    }
    let session = world.create("s_1");
    let git = untracked_repository(&world, &names);
    for (limits, reason) in [
        (
            BaseLimits {
                files: 2,
                ..BaseLimits::default()
            },
            OmitReason::TooMany,
        ),
        (
            BaseLimits {
                total_bytes: 10,
                ..BaseLimits::default()
            },
            OmitReason::TotalLimit,
        ),
    ] {
        let base = capture(
            &world.root,
            true,
            &session.store(),
            &git,
            &world.clock,
            &limits,
            &no_secrets,
        );
        assert!(!base.complete, "{base:?}");
        let kept: Vec<&str> = base
            .git
            .as_ref()
            .unwrap()
            .untracked
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(kept, ["a.txt", "b.txt"], "{reason:?}");
        let omitted: Vec<(&str, &OmitReason)> = base
            .omitted
            .iter()
            .map(|omitted| (omitted.path.as_str(), &omitted.reason))
            .collect();
        assert_eq!(omitted, [("*", &reason)], "one entry, not one a file");
    }
}

#[test]
fn without_the_whole_list_of_changed_files_no_patch_is_taken() {
    let world = World::new("base_no_name_list");
    let session = world.create("s_1");
    let failed = {
        let mut git = ScriptedGit::repository("abc123");
        git.answers.remove(3); // `diff --name-only` fails
        git
    };
    let too_long = ScriptedGit {
        too_long: vec![vec!["diff", "--name-only"]],
        ..ScriptedGit::repository("abc123")
    };
    // A protected file whose name cannot be given to git as a pathspec.
    let unnamed = {
        let mut git = ScriptedGit::repository("abc123");
        git.answers[3].1 = b"src/lib.rs\0.env\xff\0".to_vec();
        git
    };
    for (git, expected) in [
        (failed, "could not list the changed files"),
        (too_long, "the list of changed files is too long"),
        (unnamed, "not UTF-8"),
    ] {
        let base = capture(
            &world.root,
            true,
            &session.store(),
            &git,
            &world.clock,
            &BaseLimits::default(),
            &no_secrets,
        );
        assert!(!base.complete, "{base:?}");
        assert!(
            base.git.unwrap().patch.is_none(),
            "no patch without its exclusions"
        );
        let note = base.note.unwrap();
        assert!(note.contains(expected), "{note}");
        assert!(note.contains("no patch was taken"), "{note}");
        assert!(
            !git.asked()
                .iter()
                .any(|args| args[0] == "diff" && args[1] == "--no-ext-diff"),
            "the patch was not even asked for"
        );
    }
}

/// In a repository with no commits the patch holds the staged changes and
/// then the unstaged ones, and a file added with `--intent-to-add` is only
/// in the second: its name must be known to leave it out.
#[test]
fn sys_a_secret_added_with_intent_to_add_is_left_out_of_the_patch() {
    if !has_git() {
        return;
    }
    let world = World::new("sys_base_intent_to_add");
    let dir = world.workspace.clone();
    git(&dir, &["init", "--quiet"]);
    world.write("src/lib.rs", b"pub fn one() {}\n");
    world.write(".env", b"TOKEN=s3cr3t-intent-to-add\n");
    git(&dir, &["add", "src/lib.rs"]);
    git(&dir, &["add", "--intent-to-add", ".env"]);
    let session = world.create("s_1");
    let store = session.store();
    let base = capture(
        &world.root,
        true,
        &store,
        &SystemGit,
        &world.clock,
        &BaseLimits::default(),
        &no_secrets,
    );
    assert!(base.complete, "{base:?}");
    let state = base.git.clone().unwrap();
    assert!(state.commit.is_none());
    let mut patch = Vec::new();
    assert!(cortex_session::base::write_patch(&store, &state, &mut patch).unwrap());
    let patch = String::from_utf8_lossy(&patch);
    assert!(patch.contains("pub fn one"), "{patch}");
    assert!(!patch.contains("s3cr3t"), "{patch}");
    assert!(
        base.omitted
            .iter()
            .any(|omitted| omitted.path == ".env" && omitted.reason == OmitReason::Protected),
        "{:?}",
        base.omitted
    );
}

/// What a session's store kept before its first prompt goes when the
/// session ends without one, but not while a write is still under way,
/// and nothing new is stored after it ended.
#[test]
fn an_unborn_sessions_store_is_cleared_when_its_last_write_ends() {
    let world = World::new("base_unborn_in_flight");
    let session = world.create("s_1");
    let store = session.store();
    store.put_blob(b"kept while unborn").unwrap();
    let mut patch = store.begin_artifact("patch", "text/x-diff").unwrap();
    patch.write_all(b"diff --git").unwrap();
    let dir = session.dir().path().to_path_buf();
    drop(session);
    assert!(dir.exists(), "a write is still under way");
    assert!(matches!(
        store.put_blob(b"more"),
        Err(SessionError::ReadOnly { .. })
    ));
    assert!(store.begin_artifact("more", "text/plain").is_err());
    world.write("later.txt", b"later");
    assert!(
        store
            .put_file(&world.file("later.txt"), cortex_session::KEEP_LIMIT)
            .is_err()
    );
    assert!(store.put_stream(&mut &b"later"[..], 64).is_err());
    patch.finish().unwrap();
    assert!(!dir.exists(), "the last write took the directory with it");
    assert!(world.project.index().list().unwrap().is_empty());

    // A session that got its prompt keeps everything when it ends.
    let mut born = world.create("s_2");
    born.append(None, user("hello")).unwrap();
    let store = born.store();
    let hash = store.put_blob(b"kept").unwrap();
    drop(born);
    assert!(store.has_blob(&hash));
    store.put_blob(b"and more").unwrap();
}

/// A link to a file outside the workspace used to be followed, and that
/// file's content kept in the session, past what the gate holds back.
#[cfg(unix)]
#[test]
fn sys_an_untracked_link_is_recorded_as_a_link_and_never_followed() {
    if !has_git() {
        return;
    }
    let world = World::new("sys_base_link_out");
    let dir = world.workspace.clone();
    git(&dir, &["init", "--quiet"]);
    world.write("src/lib.rs", b"pub fn one() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "start"]);
    let outside = world.scratch.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let token = outside.join("token.json");
    std::fs::write(&token, b"{\"token\":\"s3cr3t-from-outside\"}").unwrap();
    std::os::unix::fs::symlink(&token, dir.join("settings.json")).unwrap();

    let session = world.create("s_1");
    let store = session.store();
    let base = capture(
        &world.root,
        true,
        &store,
        &SystemGit,
        &world.clock,
        &BaseLimits::default(),
        &no_secrets,
    );
    assert!(base.complete, "{base:?}");
    let state = base.git.unwrap();
    assert!(state.untracked.is_empty(), "{state:?}");
    assert_eq!(state.links.len(), 1, "{state:?}");
    assert_eq!(state.links[0].path, "settings.json");
    assert_eq!(state.links[0].target, token.to_string_lossy());
    assert!(!store.has_blob(&cortex_base::ContentHash::of(
        b"{\"token\":\"s3cr3t-from-outside\"}"
    )));
}

/// A link to a device used to be read for as long as the device gave
/// bytes: forever, for `/dev/zero`.
#[cfg(unix)]
#[test]
fn sys_an_untracked_link_to_a_device_does_not_stall_the_capture() {
    if !has_git() {
        return;
    }
    let world = World::new("sys_base_link_device");
    let dir = world.workspace.clone();
    git(&dir, &["init", "--quiet"]);
    world.write("src/lib.rs", b"pub fn one() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "start"]);
    std::os::unix::fs::symlink("/dev/zero", dir.join("zeros")).unwrap();

    let session = world.create("s_1");
    let capture = BaseCapture::start(
        world.root.clone(),
        true,
        session.store(),
        Arc::new(SystemGit),
        Arc::new(world.clock.clone()),
        BaseLimits::default(),
        Arc::new(no_secrets),
    )
    .unwrap();
    // A bound on a failure, not a wait: the state comes as soon as git
    // has answered.
    let base = capture
        .wait_for(Duration::from_secs(30))
        .expect("the base state is still being taken (reading /dev/zero)");
    let links = base.git.unwrap().links;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target, "/dev/zero");
}

/// The base state is taken from when a session starts, and a session that
/// never gets a prompt used to keep it, in a directory with no log.
#[test]
fn sys_a_session_that_never_got_a_prompt_leaves_nothing_behind() {
    if !has_git() {
        return;
    }
    let world = World::new("sys_base_unborn");
    let dir = world.workspace.clone();
    git(&dir, &["init", "--quiet"]);
    world.write("src/lib.rs", b"pub fn one() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "start"]);
    world.write("src/lib.rs", b"pub fn two() {}\n");
    world.write("notes.txt", b"untracked notes");
    let session = world.create("s_1");
    let base = capture(
        &world.root,
        true,
        &session.store(),
        &SystemGit,
        &world.clock,
        &BaseLimits::default(),
        &no_secrets,
    );
    assert!(base.complete, "{base:?}");
    let git_state = base.git.unwrap();
    assert!(git_state.patch.is_some() && !git_state.untracked.is_empty());
    let session_dir = session.dir().path().to_path_buf();
    assert!(session_dir.exists(), "the state was stored meanwhile");
    drop(session);
    assert!(
        !session_dir.exists(),
        "a session with no prompt left its files"
    );
    assert!(world.project.index().list().unwrap().is_empty());
}
