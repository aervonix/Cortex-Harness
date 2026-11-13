#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    reason = "tests prepare real files, and fail by panicking"
)]

use std::path::Path;

use cortex_testkit::scratch::Scratch;

use super::*;

fn workspace(test: &str) -> Scratch {
    let scratch = Scratch::outside("cortex-sandbox-protect", test);
    scratch.file("ws/.git/config", "[core]\n");
    scratch.file("ws/.git/hooks/pre-commit.sample", "#!/bin/sh\n");
    scratch.file("ws/.git/objects/ab/cdef", "blob");
    scratch.file("ws/src/main.rs", "fn main() {}\n");
    scratch
}

/// The workspace's fixed list, as the lease gives it.
fn fixed(ws: &Path) -> Vec<PathBuf> {
    [
        ".git/hooks",
        ".git/config",
        ".git/info",
        ".cortex",
        ".github/workflows",
        ".bashrc",
    ]
    .iter()
    .map(|name| ws.join(name))
    .collect()
}

fn changes(scratch: &Scratch, change: impl FnOnce(&Path)) -> Vec<(String, Change)> {
    let ws = scratch.join("ws");
    let watch = Watch::new(&ws, fixed(&ws));
    let before = watch.take();
    assert!(before.is_complete());
    change(&ws);
    let (changed, after) = watch.changes_since(&before);
    assert!(after.is_complete());
    changed
        .into_iter()
        .map(|changed| {
            let relative = changed.path.strip_prefix(&ws).unwrap();
            (
                relative.display().to_string().replace('\\', "/"),
                changed.change,
            )
        })
        .collect()
}

#[test]
fn nothing_changed_is_nothing_reported() {
    let scratch = workspace("nothing_changed");
    assert_eq!(changes(&scratch, |_| {}), Vec::new());
    // Changes outside what is protected are not reported either.
    let found = changes(&scratch, |ws| {
        fs::write(ws.join("src/main.rs"), "fn main() { x() }\n").unwrap();
        fs::write(ws.join(".git/objects/ab/new"), "blob").unwrap();
        fs::create_dir_all(ws.join("target/debug")).unwrap();
    });
    assert_eq!(found, Vec::new());
}

#[test]
fn a_hook_written_a_config_changed_and_a_file_removed_are_found() {
    let scratch = workspace("hooks_and_config");
    let found = changes(&scratch, |ws| {
        fs::write(ws.join(".git/hooks/pre-commit"), "#!/bin/sh\ncurl evil\n").unwrap();
        fs::write(ws.join(".git/config"), "[core]\nfsmonitor = ./run\n").unwrap();
        fs::remove_file(ws.join(".git/hooks/pre-commit.sample")).unwrap();
    });
    assert_eq!(
        found,
        vec![
            (".git/config".to_owned(), Change::Modified),
            (".git/hooks/pre-commit".to_owned(), Change::Created),
            (".git/hooks/pre-commit.sample".to_owned(), Change::Deleted),
        ]
    );
}

#[test]
fn what_was_there_before_is_kept_to_put_back() {
    let scratch = workspace("kept_to_put_back");
    let ws = scratch.join("ws");
    let watch = Watch::new(&ws, fixed(&ws));
    let before = watch.take();
    fs::write(ws.join(".git/config"), "[core]\nfsmonitor = ./run\n").unwrap();
    fs::create_dir_all(ws.join(".github/workflows")).unwrap();
    fs::write(ws.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
    let (changed, _) = watch.changes_since(&before);
    let config = changed
        .iter()
        .find(|changed| changed.path.ends_with(".git/config"))
        .unwrap();
    assert_eq!(config.before, Before::File(b"[core]\n".to_vec()));
    let workflow = changed
        .iter()
        .find(|changed| changed.path.ends_with("ci.yml"))
        .unwrap();
    assert_eq!(workflow.before, Before::Absent);
    assert!(
        changed
            .iter()
            .any(|changed| changed.path.ends_with(".github/workflows")
                && changed.change == Change::Created)
    );
}

// Windows keeps a change time too, which the standard library does not
// read; the check runs only where the sandbox enforces, which is Linux.
#[cfg(unix)]
#[test]
fn a_change_that_puts_the_modification_time_back_is_still_found() {
    let scratch = workspace("time_put_back");
    let ws = scratch.join("ws");
    let config = ws.join(".git/config");
    // Content too large to keep: only what the system sets, the change
    // time and the inode, can tell.
    let watch = Watch::new(&ws, fixed(&ws)).with_limits(Limits {
        file_bytes: 0,
        ..Limits::default()
    });
    let before = watch.take();
    let time = fs::metadata(&config).unwrap().modified().unwrap();
    // Same length, other content, the old time put back.
    fs::write(&config, "[xxxx]\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&config)
        .unwrap()
        .set_modified(time)
        .unwrap();
    let (changed, _) = watch.changes_since(&before);
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].change, Change::Modified);
}

#[test]
fn protected_names_are_found_at_any_depth() {
    let scratch = workspace("any_depth");
    scratch.file("ws/vendor/lib/.git/config", "[core]\n");
    scratch.file("ws/.git/modules/sub/config", "[core]\n");
    scratch.dir("ws/pkg/.cargo");
    scratch.dir("ws/bare/objects");
    scratch.dir("ws/bare/refs");
    scratch.file("ws/bare/HEAD", "ref: refs/heads/main\n");
    let found = changes(&scratch, |ws| {
        fs::write(ws.join("vendor/lib/.git/config"), "[core]\nfsmonitor = x\n").unwrap();
        fs::create_dir_all(ws.join("vendor/lib/.git/hooks")).unwrap();
        fs::write(ws.join("vendor/lib/.git/hooks/post-checkout"), "x").unwrap();
        fs::write(ws.join(".git/modules/sub/config"), "[core]\nx = 1\n").unwrap();
        fs::write(ws.join("pkg/.cargo/config.toml"), "[build]\n").unwrap();
        fs::write(ws.join("pkg/web/.npmrc"), "registry=x").unwrap_or_else(|_| {
            fs::create_dir_all(ws.join("pkg/web")).unwrap();
            fs::write(ws.join("pkg/web/.npmrc"), "registry=x").unwrap();
        });
        fs::write(ws.join("deep/a/b/.envrc"), "x").unwrap_or_else(|_| {
            fs::create_dir_all(ws.join("deep/a/b")).unwrap();
            fs::write(ws.join("deep/a/b/.envrc"), "x").unwrap();
        });
        fs::write(ws.join("bare/config"), "[core]\n").unwrap();
        fs::write(ws.join("sub.git"), "gitdir: x").unwrap();
        fs::write(ws.join("lib.git"), "not a gitlink").unwrap();
        fs::write(ws.join("other/.git"), "gitdir: elsewhere").unwrap_or_else(|_| {
            fs::create_dir_all(ws.join("other")).unwrap();
            fs::write(ws.join("other/.git"), "gitdir: elsewhere").unwrap();
        });
    });
    let paths: Vec<&str> = found.iter().map(|(path, _)| path.as_str()).collect();
    for expected in [
        "vendor/lib/.git/config",
        "vendor/lib/.git/hooks",
        "vendor/lib/.git/hooks/post-checkout",
        ".git/modules/sub/config",
        "pkg/.cargo/config.toml",
        "pkg/web/.npmrc",
        "deep/a/b/.envrc",
        "bare/config",
        "other/.git",
    ] {
        assert!(paths.contains(&expected), "{expected} not in {paths:?}");
    }
    // Names that only look alike are not protected.
    assert!(
        !paths
            .iter()
            .any(|path| path.ends_with(".git") && path.starts_with("sub"))
    );
    assert!(!paths.contains(&"lib.git"));
}

#[cfg(unix)]
#[test]
fn a_link_is_compared_and_never_followed() {
    let scratch = workspace("links");
    scratch.file("elsewhere/hook", "#!/bin/sh\n");
    let target = scratch.join("elsewhere");
    let found = changes(&scratch, |ws| {
        std::os::unix::fs::symlink(&target, ws.join(".git/hooks/pre-push")).unwrap();
        std::os::unix::fs::symlink(&target, ws.join("linked")).unwrap();
    });
    assert_eq!(
        found,
        vec![(".git/hooks/pre-push".to_owned(), Change::Created)]
    );
}

#[test]
fn a_look_that_stops_at_its_bound_reports_nothing_beyond_it() {
    let scratch = workspace("bound_both_ways");
    // Directories that come before the protected names in the walk.
    for index in 0..30 {
        scratch.dir(format!("ws/a{index:02}"));
    }
    scratch.file("ws/z/web/.npmrc", "registry=x");
    let ws = scratch.join("ws");
    let full = Watch::new(&ws, fixed(&ws));
    let capped = Watch::new(&ws, fixed(&ws)).with_limits(Limits {
        directories: 10,
        ..Limits::default()
    });
    let whole = full.take();
    let part = capped.take();
    assert!(whole.is_complete() && !part.is_complete());
    // Nothing changed: a later look that reached less sees nothing gone,
    // and one that reached more sees nothing new.
    assert_eq!(whole.compare(&part), Vec::new());
    assert_eq!(part.compare(&whole), Vec::new());
    // What both reached is still compared: the fixed list is always looked
    // at.
    fs::write(ws.join(".git/config"), "[core]\nx = 1\n").unwrap();
    let changed = whole.compare(&capped.take());
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(changed[0].path.ends_with(".git/config"));
}

#[cfg(unix)]
#[test]
fn a_linked_git_directory_is_not_looked_through() {
    let scratch = workspace("linked_git");
    scratch.file("elsewhere/config", "[core]\nsecret = 1\n");
    scratch.file("elsewhere/hooks/post-merge", "#!/bin/sh\n");
    let ws = scratch.join("ws");
    let watch = Watch::new(&ws, fixed(&ws));
    let before = watch.take();
    fs::rename(ws.join(".git"), ws.join(".git.old")).unwrap();
    std::os::unix::fs::symlink(scratch.join("elsewhere"), ws.join(".git")).unwrap();
    let (changed, after) = watch.changes_since(&before);
    assert!(after.is_complete());
    // The link is what changed, and what was under the directory is gone;
    // nothing beyond the link is looked at or reported.
    let shown: Vec<(String, Change)> = changed
        .iter()
        .map(|one| {
            let path = one.path.strip_prefix(&ws).unwrap();
            (path.display().to_string(), one.change)
        })
        .collect();
    assert!(
        shown.contains(&(".git".to_owned(), Change::Created)),
        "{shown:?}"
    );
    assert!(
        shown.contains(&(".git/config".to_owned(), Change::Deleted)),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|(path, _)| path.contains("post-merge")),
        "{shown:?}"
    );
    let config = changed
        .iter()
        .find(|one| one.path.ends_with(".git/config"))
        .unwrap();
    assert_eq!(config.before, Before::File(b"[core]\n".to_vec()));
}

#[cfg(unix)]
#[test]
fn a_change_of_mode_alone_is_found_and_cannot_be_put_back_by_content() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = workspace("mode");
    let ws = scratch.join("ws");
    let watch = Watch::new(&ws, fixed(&ws));
    let before = watch.take();
    let hook = ws.join(".git/hooks/pre-commit.sample");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let (changed, _) = watch.changes_since(&before);
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].change, Change::Modified);
    assert_eq!(changed[0].before, Before::Unknown);
    // The same bytes written again change nothing that matters.
    let before = watch.take();
    let content = fs::read(ws.join(".git/config")).unwrap();
    fs::write(ws.join(".git/config"), content).unwrap();
    assert_eq!(watch.changes_since(&before).0, Vec::new());
}

#[test]
fn past_its_limits_a_look_says_it_is_incomplete() {
    let scratch = workspace("limits");
    for index in 0..20 {
        scratch.dir(format!("ws/many/d{index}"));
    }
    let ws = scratch.join("ws");
    let limits = Limits {
        directories: 5,
        ..Limits::default()
    };
    let watch = Watch::new(&ws, fixed(&ws)).with_limits(limits);
    let before = watch.take();
    assert!(!before.is_complete());
    // A change to what was looked at is still found.
    fs::write(ws.join(".git/config"), "[core]\nx = 1\n").unwrap();
    let (changed, _) = watch.changes_since(&before);
    assert!(
        changed
            .iter()
            .any(|changed| changed.path.ends_with(".git/config"))
    );

    // Content past the limits is compared by size and times, not kept.
    let small = Limits {
        file_bytes: 3,
        ..Limits::default()
    };
    let watch = Watch::new(&ws, fixed(&ws)).with_limits(small);
    let before = watch.take();
    fs::write(ws.join(".git/config"), "[core]\ny = 22\n").unwrap();
    let (changed, _) = watch.changes_since(&before);
    let config = changed
        .iter()
        .find(|changed| changed.path.ends_with(".git/config"))
        .unwrap();
    assert_eq!(config.before, Before::Unknown);
}
