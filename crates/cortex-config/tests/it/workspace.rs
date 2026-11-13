//! Finding the workspace root (`docs/architecture/08-configuration.md`,
//! "Layers"): the nearest folder with a `.cortex` folder or a `.git` that
//! the user owns.
//!
//! Whose a candidate is cannot be shown on Unix here: no test can make a
//! folder another user owns. The unit tests in `dirs.rs` make the user up
//! instead. On Windows an administrator can hand a folder to another
//! account, which the CI runner is, and the test below does.

use std::fs;

use cortex_config::{find_workspace_root, workspace_root};

use crate::{make_link, write};

#[test]
fn the_nearest_folder_with_a_cortex_folder_or_a_git_entry_is_the_root() {
    let scratch = cortex_testkit::scratch!("root_found");

    fs::create_dir_all(scratch.join("repo/.git")).unwrap();
    fs::create_dir_all(scratch.join("repo/src/deep")).unwrap();
    assert_eq!(
        workspace_root(&scratch.join("repo/src/deep")),
        scratch.join("repo")
    );
    assert_eq!(workspace_root(&scratch.join("repo")), scratch.join("repo"));

    // A worktree's or a submodule's `.git` is a plain file. It is the user's
    // as much as a folder is.
    write(
        &scratch.join("tree/.git"),
        "gitdir: ../repo/.git/worktrees/tree\n",
    );
    fs::create_dir_all(scratch.join("tree/src")).unwrap();
    assert_eq!(
        workspace_root(&scratch.join("tree/src")),
        scratch.join("tree")
    );

    fs::create_dir_all(scratch.join("project/.cortex")).unwrap();
    fs::create_dir_all(scratch.join("project/a/b")).unwrap();
    assert_eq!(
        workspace_root(&scratch.join("project/a/b")),
        scratch.join("project")
    );

    // A `.cortex` that is a file is not a project's folder.
    write(&scratch.join("not-a-project/.cortex"), "");
    fs::create_dir_all(scratch.join("not-a-project/a")).unwrap();
    assert_ne!(
        workspace_root(&scratch.join("not-a-project/a")),
        scratch.join("not-a-project")
    );
}

/// On Unix a link at `.git` counts by its own owner, which is whoever made
/// it there. The target is not looked at.
#[cfg(unix)]
#[test]
fn on_unix_a_link_at_git_counts_by_its_own_owner() {
    let scratch = cortex_testkit::scratch!("root_link_unix");
    fs::create_dir_all(scratch.join("ws/sub")).unwrap();
    // A link to nothing: an owner read through it would find nothing to
    // read, so only the link's own owner can make it count.
    assert!(make_link(
        &scratch.join("nowhere"),
        &scratch.join("ws/.git"),
        true
    ));
    assert_eq!(workspace_root(&scratch.join("ws/sub")), scratch.join("ws"));

    // A `.cortex` must be a folder: a link there is passed over, and said.
    fs::create_dir_all(scratch.join("elsewhere")).unwrap();
    fs::create_dir_all(scratch.join("project/sub")).unwrap();
    assert!(make_link(
        &scratch.join("elsewhere"),
        &scratch.join("project/.cortex"),
        true
    ));
    let found = find_workspace_root(&scratch.join("project/sub"));
    assert_ne!(found.root, scratch.join("project"));
    assert_eq!(found.passed_over[0], scratch.join("project/.cortex"));
}

#[cfg(windows)]
mod windows {
    use std::path::Path;

    use super::*;

    fn in_ci() -> bool {
        std::env::var_os("GITHUB_ACTIONS").is_some()
    }

    /// On Windows a link or a junction at `.git` is passed over: its owner
    /// cannot be read without following it, and following a link can
    /// reach a network share.
    #[test]
    fn a_link_at_git_is_passed_over() {
        let scratch = cortex_testkit::scratch!("root_link_windows");
        fs::create_dir_all(scratch.join("elsewhere")).unwrap();
        fs::create_dir_all(scratch.join("ws/sub")).unwrap();
        if !make_link(&scratch.join("elsewhere"), &scratch.join("ws/.git"), true) {
            return;
        }
        let found = find_workspace_root(&scratch.join("ws/sub"));
        assert_ne!(found.root, scratch.join("ws"));
        assert_eq!(found.passed_over[0], scratch.join("ws").join(".git"));
    }

    /// Gives `path` to another owner with `icacls`, which needs the restore
    /// privilege that an administrator has. `false` when it could not.
    fn set_owner(path: &Path, owner: &str) -> bool {
        let output = std::process::Command::new("icacls")
            .arg(path)
            .arg("/setowner")
            .arg(owner)
            .output()
            .expect("run icacls");
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stdout).into_owned();
            assert!(!in_ci(), "the CI runner could not set an owner: {said}");
            println!("skipped: cannot set an owner here ({said})");
        }
        output.status.success()
    }

    /// A folder this test made counts: it is the user's, or the
    /// Administrators group's when the test runs elevated, as the CI
    /// runner does. Handed to the group, it still counts. Handed to SYSTEM,
    /// standing in for any other account, it does not.
    #[test]
    fn sys_a_candidate_owned_by_another_account_is_passed_over() {
        let scratch = cortex_testkit::scratch!("root_owner_windows");
        for (name, owner, counts) in [
            ("made", None, true),
            ("administrators", Some("*S-1-5-32-544"), true),
            ("system", Some("*S-1-5-18"), false),
            ("users", Some("*S-1-5-32-545"), false),
        ] {
            let ws = scratch.join(name);
            fs::create_dir_all(ws.join(".git")).unwrap();
            fs::create_dir_all(ws.join("sub")).unwrap();
            if let Some(owner) = owner
                && !set_owner(&ws.join(".git"), owner)
            {
                return;
            }
            let found = find_workspace_root(&ws.join("sub"));
            let root = found.root;
            if counts {
                assert_eq!(root, ws, "{name}");
            } else {
                assert_ne!(root, ws, "{name}");
                assert_eq!(found.passed_over[0], ws.join(".git"), "{name}");
            }
        }
    }
}
