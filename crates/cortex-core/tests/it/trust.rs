//! One answer about trust, given to the four places that need it
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Project trust").
//!
//! Each test sets a project up so that trust shows in four ways at once,
//! runs one turn, and reads all four:
//!
//! | Consumer | What a trusted project gets |
//! |---|---|
//! | Configuration | Its own `model.main`, which raises cost and so waits for trust. |
//! | The permission engine | Its `allow` rule: a write under `.github/` is not asked about. |
//! | The shell tool | Git is asked what a command changed (`changes.method` is `git`). |
//! | The base state | A complete record, marked as taken from a trusted project. |
//!
//! The tests start a shell and git, so they are named `sys_…`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use cortex_base::{PathOptions, PathResolver};
use cortex_core::CoreOptions;
use cortex_policy::{Definition, TrustStore};
use cortex_protocol::Item;
use cortex_protocol::lease::{Effect, Rule, RuleAction};
use cortex_protocol::methods::ApprovalDetail;
use cortex_protocol::model::ModelRole;
use serde_json::{Value, json};

use crate::{Front, MODEL, OTHER_MODEL, World, calls, says};

const PROTECTED: &str = ".github/workflows/ci.yml";

fn git(dir: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .current_dir(dir)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn allow_rule() -> Rule {
    Rule {
        effect: Effect::Write,
        pattern: ".github/**".to_owned(),
        action: RuleAction::Allow,
    }
}

/// A project that asks for a dearer model and allows itself a protected
/// write, in a repository with one committed file. `None` where there is
/// no git to make the repository with.
fn project(test: &str) -> Option<World> {
    let world = World::new(test, []);
    world.project_config(&format!(
        "[model]\nmain = \"{OTHER_MODEL}\"\n\n[[permissions.rules]]\neffect = \"write\"\nmatch = \
         \".github/**\"\naction = \"allow\"\n"
    ));
    world.put("tracked.txt", "one\n");
    let repository = git(&world.workspace, &["init", "-q"])
        && git(&world.workspace, &["add", "."])
        && git(&world.workspace, &["commit", "-q", "-m", "first"]);
    repository.then_some(world)
}

/// What one turn showed of the project's trust.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    /// The model the session runs on.
    model: String,
    /// Whether the write the project's rule allows was asked about.
    write_asked: bool,
    /// Whether `git status` was asked about. The engine takes git for a
    /// command that only reads in a trusted project alone: the project's
    /// own configuration of git can make any git command run a program.
    git_asked: bool,
    /// How the shell tool looked for what the command changed.
    method: String,
    /// Whether the checkpoint has the earlier content of the file the
    /// command changed, which only git can give.
    restorable: bool,
    /// Whether the base state was taken as a trusted project's.
    base_trusted: bool,
    base_complete: bool,
}

impl Seen {
    fn trusted() -> Self {
        Self {
            model: OTHER_MODEL.to_owned(),
            write_asked: false,
            git_asked: false,
            method: "git".to_owned(),
            restorable: true,
            base_trusted: true,
            base_complete: true,
        }
    }

    fn untrusted() -> Self {
        Self {
            model: MODEL.to_owned(),
            write_asked: true,
            git_asked: true,
            method: "mtime".to_owned(),
            restorable: false,
            base_trusted: false,
            base_complete: false,
        }
    }
}

/// Runs one turn in the project and reads the four consumers. Returns the
/// notices the session gave as well.
fn observe(world: &World, change: impl FnOnce(&mut CoreOptions)) -> (Seen, Vec<String>) {
    world.provider.push(calls(&[(
        "c1",
        "write",
        json!({"path": PROTECTED, "content": "on: push\n"}),
    )]));
    world.provider.push(calls(&[(
        "c2",
        "shell",
        json!({"command": "echo more >> tracked.txt"}),
    )]));
    world.provider.push(calls(&[(
        "c3",
        "shell",
        json!({"command": "git status --short"}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| {
        options.base_capture = true;
        change(options);
    });
    world.run(async {
        let front = Front::start(world, &core).await;
        let model = front.state.roles[&ModelRole::Main]
            .model
            .as_str()
            .to_owned();
        let asked = front.turn_allowing("go").await;
        let method = front
            .items()
            .iter()
            .find_map(|item| match item {
                Item::ToolResult(result) if result.tool == "shell" => {
                    let payload = result.payload.as_ref()?;
                    assert_eq!(payload["exit_code"], 0, "{payload}");
                    Some(payload["changes"]["method"].as_str()?.to_owned())
                }
                _ => None,
            })
            .expect("a shell result with what it changed");
        // Which command was asked about is read from the question itself:
        // what else a shell asks about differs from one shell to another.
        let git_asked = front.approvals().iter().any(|request| {
            matches!(
                &request.detail,
                ApprovalDetail::Command(command) if command.command.starts_with("git status")
            )
        });
        let notices = front.notices();
        front.close().await;

        let lines: Vec<Value> = front.log(world);
        let base = lines
            .iter()
            .find(|line| line["kind"] == "base")
            .expect("the base state was recorded");
        // The checkpoint of the command's round names the file, with its
        // earlier content only when git was asked for it.
        let restorable = lines.iter().any(|line| {
            line["kind"] == "checkpoint"
                && line["files"].as_array().is_some_and(|files| {
                    files
                        .iter()
                        .any(|file| file["path"] == "tracked.txt" && file["before"].is_object())
                })
        });
        let unknown = lines.iter().any(|line| {
            line["kind"] == "checkpoint"
                && line["files"].as_array().is_some_and(|files| {
                    files
                        .iter()
                        .any(|file| file["path"] == "tracked.txt" && file["before"] == "unknown")
                })
        });
        assert!(
            restorable ^ unknown,
            "the command's change is in a checkpoint: {lines:?}"
        );
        (
            Seen {
                model,
                write_asked: asked.contains(&"write".to_owned()),
                git_asked,
                method,
                restorable,
                base_trusted: base["trusted"] == true,
                base_complete: base["complete"] == true,
            },
            notices,
        )
    })
}

fn trust_store(world: &World) -> (TrustStore, cortex_protocol::ProjectKey) {
    let state = world.dirs().state.unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let store = TrustStore::new(state.join("trust.toml"), Arc::new(world.clock.clone()));
    let key = PathResolver::new(&world.workspace, PathOptions::new())
        .unwrap()
        .root()
        .project_key();
    (store, key)
}

#[test]
fn sys_an_untrusted_project_gets_nothing_that_needs_trust() {
    let Some(world) = project("trust_none") else {
        return;
    };
    let (seen, notices) = observe(&world, |_| {});
    assert_eq!(seen, Seen::untrusted());
    // What was held back is said, by name.
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("not trusted") && notice.contains("model.main")),
        "{notices:?}"
    );
}

#[test]
fn sys_trust_for_the_run_reaches_all_four() {
    let Some(world) = project("trust_flag") else {
        return;
    };
    let (seen, _) = observe(&world, |options| options.trust_project = true);
    assert_eq!(seen, Seen::trusted());
}

#[test]
fn sys_a_project_trusted_in_the_store_is_trusted_while_its_rules_are_the_ones_the_user_saw() {
    let Some(world) = project("trust_store") else {
        return;
    };
    let (store, key) = trust_store(&world);
    store
        .trust(&key, "~/work/app", &[Definition::of_rules(&[allow_rule()])])
        .unwrap();
    let (seen, notices) = observe(&world, |_| {});
    assert_eq!(seen, Seen::trusted(), "{notices:?}");

    // The project's rules change behind the user's back: it allows itself
    // more. Nothing of it is trusted until the user has looked again.
    world.project_config(&format!(
        "[model]\nmain = \"{OTHER_MODEL}\"\n\n[[permissions.rules]]\neffect = \"write\"\nmatch = \
         \".github/**\"\naction = \"allow\"\n\n[[permissions.rules]]\neffect = \"exec\"\nmatch = \
         \"curl *\"\naction = \"allow\"\n"
    ));
    std::fs::remove_file(world.workspace.join(PROTECTED)).unwrap();
    let (seen, notices) = observe(&world, |_| {});
    assert_eq!(seen, Seen::untrusted());
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("rules have changed")),
        "{notices:?}"
    );
}

#[test]
fn sys_a_trust_store_that_cannot_be_read_trusts_nothing_and_says_so() {
    let Some(world) = project("trust_unreadable") else {
        return;
    };
    let state = world.dirs().state.unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("trust.toml"), "this is { not toml").unwrap();
    let (seen, notices) = observe(&world, |_| {});
    assert_eq!(seen, Seen::untrusted());
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("trust store could not be read")),
        "{notices:?}"
    );
}
