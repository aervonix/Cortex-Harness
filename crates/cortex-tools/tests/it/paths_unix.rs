//! Paths that only a Unix file system can hold: names that are not Unicode,
//! and symbolic links made without special rights.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;

use cortex_tools::{Effect, ListTool, ReadTool, SearchTool};
use serde_json::json;

use crate::{Workspace, failed, ok};

/// A file whose name is not valid UTF-8, if this file system takes one.
/// Some do not (APFS, and file systems mounted with a UTF-8 rule); the test
/// then has nothing to check.
fn non_unicode_file(ws: &Workspace, dir: &str, content: &str) -> bool {
    let dir = ws.dir(dir);
    let name = OsStr::from_bytes(b"caf\xe9.txt");
    fs::write(dir.join(name), content).is_ok()
}

#[test]
fn a_listing_steps_past_a_name_that_is_not_unicode_and_says_so() {
    let ws = Workspace::new("unix_list_non_unicode");
    ws.file("docs/a.txt", "");
    ws.file("docs/z.txt", "");
    if !non_unicode_file(&ws, "docs", "") {
        println!("skipped: this file system refuses names that are not UTF-8");
        return;
    }
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({"path": "docs"}))),
        format!(
            "2 entries in `docs`:\ndocs/a.txt\ndocs/z.txt\n\
             [Skipped 1 entry whose path cannot be used. The first: the path `{}/caf\u{fffd}.txt` \
             is not valid Unicode, so it cannot be checked against any rule]",
            ws.paths.resolve("docs").unwrap().abs()
        )
    );
}

#[test]
fn a_search_steps_past_a_name_that_is_not_unicode_and_still_finds_the_rest() {
    let ws = Workspace::new("unix_search_non_unicode");
    ws.file("docs/a.txt", "needle\n");
    ws.file("docs/sub/z.txt", "needle\n");
    if !non_unicode_file(&ws, "docs", "needle\n") {
        println!("skipped: this file system refuses names that are not UTF-8");
        return;
    }
    // A directory with such a name is stepped past with everything in it.
    let bad_dir = ws.root().join(OsStr::from_bytes(b"bad\xff"));
    fs::create_dir(&bad_dir).unwrap();
    fs::write(bad_dir.join("inside.txt"), "needle\n").unwrap();

    let text = ok(&ws.call(
        &SearchTool::new(),
        json!({"pattern": "needle", "mode": "files"}),
    ));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        &lines[..3],
        [
            "2 files with a match for `needle` in `.`:",
            "docs/a.txt",
            "docs/sub/z.txt"
        ]
    );
    assert_eq!(lines.len(), 4, "{text}");
    assert!(
        lines[3].starts_with("[Skipped 2 entries whose path cannot be used. The first: the path `"),
        "{text}"
    );
    assert!(lines[3].ends_with("is not valid Unicode, so it cannot be checked against any rule]"));
}

#[test]
fn a_file_reached_through_a_link_is_read_at_its_real_location() {
    let ws = Workspace::new("unix_read_link");
    ws.file("real/notes.txt", "inside\n");
    fs::write(ws.dir.join("outside.txt"), "outside\n").unwrap();
    symlink("real/notes.txt", ws.root().join("inside-link")).unwrap();
    symlink(ws.dir.join("outside.txt"), ws.root().join("outside-link")).unwrap();
    let tool = ReadTool::new();

    // A link to a file in the workspace: the effect, and the header, name
    // the file, so a rule on the file cannot be dodged through the link.
    let plan = ws.plan(&tool, json!({"path": "inside-link"})).unwrap();
    assert!(
        matches!(plan.effects.as_slice(), [Effect::Read(path)] if path.display() == "real/notes.txt")
    );
    assert_eq!(
        ok(&ws.call(&tool, json!({"path": "inside-link"}))),
        "real/notes.txt (1 line)\ninside"
    );
    // Read through the link and by name, it is one seen file.
    assert!(
        ok(&ws.call(&tool, json!({"path": "real/notes.txt"})))
            .starts_with("real/notes.txt: unchanged")
    );
    assert_eq!(ws.files.seen().len(), 1);

    // A link that leaves the workspace: the effect says it is outside.
    let plan = ws.plan(&tool, json!({"path": "outside-link"})).unwrap();
    let [Effect::Read(path)] = plan.effects.as_slice() else {
        panic!("one read, not {:?}", plan.effects);
    };
    assert!(!path.is_inside_workspace());
    assert!(path.display().ends_with("/unix_read_link/outside.txt"));
}

#[test]
fn a_link_to_nothing_is_refused_with_the_resolvers_reason() {
    let ws = Workspace::new("unix_dangling");
    symlink("gone.txt", ws.root().join("dangling")).unwrap();
    let text = failed(&ws.call(&ReadTool::new(), json!({"path": "dangling"})));
    assert!(
        text.ends_with("/dangling` is a symbolic link to something that does not exist"),
        "{text}"
    );
    assert!(
        ws.plan(&ReadTool::new(), json!({"path": "dangling"}))
            .is_err()
    );
}

#[test]
fn a_walk_does_not_follow_links_and_says_what_it_left() {
    let ws = Workspace::new("unix_walk_links");
    ws.file("src/main.rs", "needle\n");
    ws.file("elsewhere/far.rs", "needle\n");
    fs::create_dir_all(ws.dir.join("outside")).unwrap();
    fs::write(ws.dir.join("outside/secret.rs"), "needle\n").unwrap();
    symlink(ws.dir.join("outside"), ws.root().join("src/out")).unwrap();
    symlink("../elsewhere/far.rs", ws.root().join("src/far-link.rs")).unwrap();
    symlink("gone", ws.root().join("src/dangling")).unwrap();
    // A link that points back up would make a walk that followed it loop.
    symlink("..", ws.root().join("src/loop")).unwrap();

    // The search stays in the tree it was planned for.
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "path": "src", "mode": "files"})
        )),
        "1 file with a match for `needle` in `src`:\nsrc/main.rs\n\
         [Skipped 4 symbolic links: a walk does not follow links. Name one as `path` to use \
         what it points to.]"
    );
    // Named as the path, a link is followed: by the resolver, by its rules.
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "path": "src/far-link.rs", "mode": "files"})
        )),
        "1 file with a match for `needle` in `elsewhere/far.rs`:\nelsewhere/far.rs"
    );

    // A listing shows a link as what it leads to, and does not go in. The
    // dangling one is refused, as it is everywhere.
    let text = ok(&ws.call(&ListTool::new(), json!({"path": "src", "depth": 5})));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        &lines[..5],
        [
            "4 entries in `src`:",
            "src/far-link.rs",
            "src/loop/",
            "src/main.rs",
            "src/out/"
        ]
    );
    assert_eq!(lines.len(), 6, "{text}");
    assert!(
        lines[5].starts_with("[Skipped 1 entry whose path cannot be used. The first: `"),
        "{text}"
    );
    assert!(
        lines[5].ends_with("/src/dangling` is a symbolic link to something that does not exist]")
    );
}

#[test]
fn names_that_only_windows_refuses_are_ordinary_here() {
    let ws = Workspace::new("unix_windows_names");
    ws.file("notes:2026", "needle\n");
    ws.file("name.", "needle\n");
    ws.file("aux", "needle\n");
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "mode": "files"})
        )),
        "3 files with a match for `needle` in `.`:\naux\nname.\nnotes:2026"
    );
    assert_eq!(
        ok(&ws.call(&ReadTool::new(), json!({"path": "notes:2026"}))),
        "notes:2026 (1 line)\nneedle"
    );
}

#[test]
fn a_file_that_cannot_be_opened_is_skipped_by_a_search_and_refused_by_a_read() {
    use std::os::unix::fs::PermissionsExt;
    let ws = Workspace::new("unix_unreadable");
    ws.file("open.txt", "needle\n");
    let locked = ws.file("locked.txt", "needle\n");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&locked).is_ok() {
        println!("skipped: running as a user that file permissions do not stop");
        return;
    }
    let text = ok(&ws.call(
        &SearchTool::new(),
        json!({"pattern": "needle", "mode": "files"}),
    ));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        &lines[..2],
        ["1 file with a match for `needle` in `.`:", "open.txt"]
    );
    assert!(
        lines[2].starts_with("[Skipped 1 entry that could not be read. The first: locked.txt: "),
        "{text}"
    );
    let text = failed(&ws.call(&ReadTool::new(), json!({"path": "locked.txt"})));
    assert!(
        text.starts_with("`locked.txt` could not be read: "),
        "{text}"
    );
}
