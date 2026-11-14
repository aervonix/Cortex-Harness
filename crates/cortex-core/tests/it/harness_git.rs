//! The harness's own git, inside the sandbox while commands are confined
//! (`docs/architecture/06-permissions-and-sandbox.md`, "The harness's own
//! git"; found by the third review of step 26).
//!
//! A confined command may write a repository's configuration: it lies in
//! the workspace. What it writes can name a program for git to run (here a
//! clean filter, which `git status` and `git diff` run on a file whose
//! times no longer match the index). The look for what a command changed,
//! the base state and a file's earlier content all ask git, so each git
//! the harness starts runs inside the sandbox, with no write root and no
//! network: the filter runs confined, as the command did.
//!
//! Every test here starts a shell or git, so each is named `sys_…`, and
//! each needs a kernel where the sandbox enforces.

#![cfg(target_os = "linux")]

use std::path::Path;

use cortex_protocol::Item;
use cortex_protocol::lease::Level;
use cortex_protocol::session::{SandboxStatus, SessionSettings};
use cortex_testkit::scratch::git;
use serde_json::{Value, json};

use crate::sandbox::{enforcing_here, launcher};
use crate::{Front, World, calls, says};

/// A session at `level`.
fn at(level: Level) -> SessionSettings {
    SessionSettings {
        level: Some(level),
        ..SessionSettings::default()
    }
}

/// The world's workspace as a repository with one commit of `tracked.txt`.
fn repository(world: &World) {
    world.put("tracked.txt", "one\n");
    git(
        &world.workspace,
        &["init", "--quiet", "--initial-branch=main"],
    );
    git(&world.workspace, &["add", "--all"]);
    git(
        &world.workspace,
        &["commit", "--quiet", "--message", "first"],
    );
}

/// Names `clean` as the clean filter of every file in the repository's
/// configuration, and makes the tracked file's times no longer match the
/// index, so that git must read it again through the filter.
fn plant_filter(world: &World, clean: &str, required: bool) {
    let config = world.workspace.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[filter \"x\"]\n\tclean = {clean}\n\trequired = {required}\n"
    ));
    std::fs::write(&config, text).unwrap();
    world.put(".gitattributes", "* filter=x\n");
    stale(&world.workspace.join("tracked.txt"));
}

/// Sets a file's modification time to the epoch.
fn stale(file: &Path) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
}

/// The payload of the first `shell` result.
fn shell_payload(front: &Front) -> Value {
    front
        .items()
        .iter()
        .find_map(|item| match item {
            Item::ToolResult(result) if result.tool == "shell" => result.payload.clone(),
            _ => None,
        })
        .expect("a shell result with a payload")
}

/// The review's case: a confined command plants a filter in a trusted
/// repository's configuration, and the look that follows it asks git what
/// changed. The filter runs, inside the sandbox, and cannot write outside;
/// the look at protected paths then stops the turn.
#[test]
fn sys_a_filter_a_confined_command_plants_runs_inside_the_sandbox() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("planted_filter", []);
    repository(&world);
    let outside = world.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // A script, so that the command line names no protected path and
    // nothing is asked.
    world.put(
        "fix.sh",
        format!(
            "echo direct > '{o}/direct'\n\
             printf '[filter \"x\"]\\n\\tclean = touch {o}/planted; cat\\n' >> .git/config\n\
             echo '* filter=x' > .gitattributes\n\
             touch -d @0 tracked.txt\n",
            o = outside.display()
        ),
    );
    world.provider.push(says("Ready."));
    world
        .provider
        .push(calls(&[("c1", "shell", json!({"command": "sh fix.sh"}))]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.trust_project = true;
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("the session starts");
        assert_eq!(front.state.sandbox, SandboxStatus::Enforcing);
        front.turn("get ready").await;
        front.turn("go").await;
        let results = front.results();
        assert!(
            !outside.join("direct").exists(),
            "the confined command wrote outside: {results:?}"
        );
        assert!(
            results[0].2.contains("changed protected paths"),
            "{results:?}"
        );
        // The look asked git, and git asked the filter.
        assert_eq!(shell_payload(&front)["changes"]["method"], "git");
        assert!(
            !outside.join("planted").exists(),
            "the planted filter ran outside the sandbox: {results:?}"
        );
        front.close().await;
    });
}

/// The same filter, already in the configuration when the session starts:
/// the base state asks git about every changed file, and its git is the
/// sandbox's too. At `unrestricted`, where nothing is confined, the same
/// filter does run: that is what shows it is one git runs.
#[test]
fn sys_the_base_state_is_taken_by_a_git_inside_the_sandbox() {
    if !enforcing_here() {
        return;
    }
    let ran = |level: Level| -> bool {
        let world = World::new(
            if level == Level::Unrestricted {
                "base_filter_unconfined"
            } else {
                "base_filter_confined"
            },
            [],
        );
        repository(&world);
        let marker = world.dir.join("filter-ran");
        plant_filter(&world, &format!("touch {}; cat", marker.display()), false);
        world.provider.push(says("Hello."));
        let core = world.core_with(|options| {
            options.launcher = Some(launcher());
            options.trust_project = true;
            options.base_capture = true;
        });
        world.run(async {
            let front = Front::start_as(&world, &core, cortex_core::ClientInfo::user(), at(level))
                .await
                .expect("the session starts");
            front.turn("hello").await;
            // Closing waits for the base state.
            front.close().await;
            let lines = front.log(&world);
            let base = lines
                .iter()
                .find(|line| line["kind"] == "base")
                .expect("the base state was recorded");
            assert_eq!(base["trusted"], true, "{base}");
        });
        marker.exists()
    };
    assert!(
        !ran(Level::Autonomous),
        "the base state's git ran a filter outside the sandbox"
    );
    assert!(
        ran(Level::Unrestricted),
        "the control: at `unrestricted` git runs the filter"
    );
}

/// A filter that must write (as git-lfs's does) fails inside the sandbox,
/// so git gives no answer: the list of what the command changed then says
/// it is incomplete, and why, in the result's payload and its text.
#[test]
fn sys_a_git_that_fails_inside_the_sandbox_leaves_the_list_incomplete_and_says_why() {
    if !enforcing_here() {
        return;
    }
    let world = World::new("failing_filter", []);
    repository(&world);
    plant_filter(&world, "touch .git/filter-wrote && cat", true);
    world.provider.push(calls(&[(
        "c1",
        "shell",
        json!({"command": "echo new > new.txt"}),
    )]));
    world.provider.push(says("Done."));
    let core = world.core_with(|options| {
        options.launcher = Some(launcher());
        options.trust_project = true;
    });
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            cortex_core::ClientInfo::user(),
            at(Level::Autonomous),
        )
        .await
        .expect("the session starts");
        front.turn("go").await;
        let results = front.results();
        let changes = &shell_payload(&front)["changes"];
        assert_eq!(changes["complete"], false, "{changes}");
        assert_eq!(
            changes["incomplete"],
            cortex_tools::shell::Incomplete::GitFailedConfined.as_str(),
            "{changes}"
        );
        assert!(
            results[0]
                .2
                .contains("The list of files the command changed is incomplete: git exited"),
            "{results:?}"
        );
        assert_eq!(world.read("new.txt").as_deref(), Some("new\n"));
        assert!(
            !world.workspace.join(".git/filter-wrote").exists(),
            "the harness's git wrote in the workspace"
        );
        front.close().await;
    });
}
