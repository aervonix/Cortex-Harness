//! Resolving paths on Linux: symbolic links, exact case, and names that
//! only Windows objects to.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;

use cortex_base::PathError;

use crate::Scratch;

#[test]
fn a_link_that_leaves_the_workspace_resolves_to_where_it_leads() {
    let scratch = Scratch::new("unix_link_out");
    scratch.file("outside/secret.txt", "s");
    scratch.dir("ws");
    symlink(scratch.join("outside"), scratch.join("ws/vendor")).unwrap();
    symlink(
        scratch.join("outside/secret.txt"),
        scratch.join("ws/notes.txt"),
    )
    .unwrap();
    let resolver = scratch.resolver("ws");

    // The check applies to the target, so the target is what comes back.
    for (written, real) in [
        ("vendor", "outside"),
        ("vendor/secret.txt", "outside/secret.txt"),
        ("notes.txt", "outside/secret.txt"),
        ("./vendor/./secret.txt", "outside/secret.txt"),
    ] {
        let resolved = resolver.resolve(written).unwrap();
        assert_eq!(resolved.abs(), &scratch.real(real), "{written:?}");
        assert!(!resolved.is_inside_workspace(), "{written:?} is outside");
        assert!(resolved.exists());
    }

    // A file that does not exist yet, in a directory reached through the
    // link, would be created outside.
    let resolved = resolver.resolve("vendor/new.txt").unwrap();
    assert!(!resolved.exists());
    assert!(!resolved.is_inside_workspace());
    assert_eq!(
        resolved.abs(),
        &scratch.real("outside").join("new.txt").unwrap()
    );
    let resolved = resolver.resolve("vendor/a/b/new.txt").unwrap();
    assert_eq!(
        resolved.abs(),
        &scratch.real("outside").join("a/b/new.txt").unwrap()
    );
}

#[test]
fn a_link_that_stays_inside_resolves_to_the_real_file_inside() {
    let scratch = Scratch::new("unix_link_in");
    scratch.file("ws/real/file.txt", "x");
    symlink("real", scratch.join("ws/alias")).unwrap();
    symlink("alias", scratch.join("ws/alias2")).unwrap();
    symlink("../real/file.txt", scratch.join("ws/real/self.txt")).unwrap();
    let resolver = scratch.resolver("ws");

    for written in [
        "alias/file.txt",
        "alias2/file.txt",
        "real/self.txt",
        "alias2/self.txt",
    ] {
        let resolved = resolver.resolve(written).unwrap();
        assert!(resolved.is_inside_workspace(), "{written:?}");
        assert_eq!(resolved.display(), "real/file.txt", "{written:?}");
        assert_eq!(resolved, resolver.resolve("real/file.txt").unwrap());
    }
    assert_eq!(
        resolver.resolve("alias/new.txt").unwrap().display(),
        "real/new.txt"
    );
}

#[test]
fn dot_dot_after_a_link_is_the_parent_of_the_links_target() {
    let scratch = Scratch::new("unix_link_dot_dot");
    scratch.file("outside/deep/file.txt", "x");
    scratch.file("outside/beside.txt", "x");
    scratch.file("ws/beside.txt", "x");
    symlink(scratch.join("outside/deep"), scratch.join("ws/link")).unwrap();
    let resolver = scratch.resolver("ws");

    // Read as text, `link/..` would be the workspace. The kernel goes to
    // the parent of where the link leads, and so does the resolver.
    let resolved = resolver.resolve("link/../beside.txt").unwrap();
    assert_eq!(resolved.abs(), &scratch.real("outside/beside.txt"));
    assert!(!resolved.is_inside_workspace());

    // The same when the last name does not exist yet.
    let resolved = resolver.resolve("link/../new.txt").unwrap();
    assert_eq!(
        resolved.abs(),
        &scratch.real("outside").join("new.txt").unwrap()
    );
    assert!(!resolved.is_inside_workspace());
    assert!(!resolved.exists());
}

#[test]
fn a_link_to_nothing_is_refused() {
    let scratch = Scratch::new("unix_dangling");
    scratch.dir("ws/dir");
    symlink(scratch.join("nowhere/at/all"), scratch.join("ws/dangling")).unwrap();
    symlink("missing-sibling", scratch.join("ws/dir/dangling")).unwrap();
    let resolver = scratch.resolver("ws");

    for written in [
        "dangling",
        "dangling/file.txt",
        "dir/dangling",
        "dir/../dangling/x/y",
    ] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::DanglingLink { .. }),
            "{written:?}: {error}"
        );
        assert!(error.to_string().contains("dangling"), "{error}");
    }
}

#[test]
fn a_loop_of_links_is_refused() {
    let scratch = Scratch::new("unix_loop");
    scratch.dir("ws");
    symlink("b", scratch.join("ws/a")).unwrap();
    symlink("a", scratch.join("ws/b")).unwrap();
    let resolver = scratch.resolver("ws");
    for written in ["a", "a/file.txt", "b/../a"] {
        assert!(
            matches!(resolver.resolve(written), Err(PathError::Io { .. })),
            "{written:?}"
        );
    }
}

#[test]
fn a_workspace_opened_through_a_link_is_known_by_its_real_location() {
    let scratch = Scratch::new("unix_root_link");
    scratch.file("real-ws/src/lib.rs", "x");
    symlink(scratch.join("real-ws"), scratch.join("ws-link")).unwrap();
    let resolver =
        cortex_base::PathResolver::new(&scratch.join("ws-link"), cortex_base::PathOptions::new())
            .unwrap();
    assert_eq!(resolver.root(), &scratch.real("real-ws"));

    // The file is inside whichever way its path is written.
    let through_link = scratch.join("ws-link/src/lib.rs");
    let direct = scratch.join("real-ws/src/lib.rs");
    for written in [
        through_link.to_str().unwrap(),
        direct.to_str().unwrap(),
        "src/lib.rs",
    ] {
        let resolved = resolver.resolve(written).unwrap();
        assert!(resolved.is_inside_workspace(), "{written:?}");
        assert_eq!(resolved.display(), "src/lib.rs");
    }
}

#[test]
fn case_is_exact() {
    let scratch = Scratch::new("unix_case");
    scratch.file("ws/src/Main.rs", "upper");
    let resolver = scratch.resolver("ws");
    assert!(resolver.resolve("src/Main.rs").unwrap().exists());
    let other = resolver.resolve("src/main.rs").unwrap();
    assert!(!other.exists(), "a different name, which does not exist");
    assert_ne!(other, resolver.resolve("src/Main.rs").unwrap());
    assert_ne!(
        resolver.resolve("SRC/Main.rs").unwrap(),
        resolver.resolve("src/Main.rs").unwrap()
    );
}

#[test]
fn names_that_only_windows_refuses_are_ordinary_files() {
    let scratch = Scratch::new("unix_windows_names");
    let names = [
        "notes:2026",
        "name.",
        "trailing ",
        "NUL",
        "con.txt",
        "COM1",
        "what?.txt",
        "a*b",
        "a|b",
        "a<b>c",
        "say \"hi\"",
        "back\\slash",
        "...",
        "C:",
        "tab\there",
    ];
    for name in names {
        scratch.file(&format!("ws/{name}"), "x");
    }
    let resolver = scratch.resolver("ws");
    for name in names {
        let resolved = resolver.resolve(name).unwrap();
        assert!(resolved.exists(), "{name:?}");
        assert!(resolved.is_inside_workspace());
        assert_eq!(resolved.display(), name);
        assert_eq!(resolved.abs().file_name(), Some(name));
    }
    assert_eq!(scratch.listing("ws").len(), names.len());
}

#[test]
fn a_name_that_is_not_unicode_is_refused_alone() {
    let scratch = Scratch::new("unix_not_unicode");
    scratch.file("ws/dir/good.txt", "x");
    let odd_name = OsStr::from_bytes(b"caf\xe9.txt");
    std::fs::write(scratch.join("ws/dir").join(odd_name), "x").unwrap();
    symlink(odd_name, scratch.join("ws/dir/link-to-odd")).unwrap();
    let resolver = scratch.resolver("ws");

    // A directory walk meets the odd name, gets a refusal of that one
    // entry, and goes on to the others.
    let directory = resolver.resolve("dir").unwrap();
    let mut accepted = Vec::new();
    let mut refused = 0;
    for entry in std::fs::read_dir(directory.to_fs_path()).unwrap() {
        match directory.abs().child(&entry.unwrap().file_name()) {
            Ok(child) => accepted.push(child.file_name().unwrap().to_owned()),
            Err(PathError::NotUnicode { path }) => {
                assert!(path.contains("caf"), "the message names the entry: {path}");
                refused += 1;
            }
            Err(error) => panic!("unexpected refusal: {error}"),
        }
    }
    accepted.sort();
    assert_eq!(accepted, ["good.txt", "link-to-odd"]);
    assert_eq!(refused, 1);

    // Reaching it through a link is refused too, as a refusal and not a
    // panic: the real location cannot be written as text.
    assert!(matches!(
        resolver.resolve("dir/link-to-odd"),
        Err(PathError::NotUnicode { .. })
    ));
    // Everything else in the directory still resolves.
    assert!(resolver.resolve("dir/good.txt").unwrap().exists());
}

#[test]
fn a_directory_that_cannot_be_entered_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("unix_no_permission");
    scratch.file("ws/locked/file.txt", "x");
    let locked = scratch.join("ws/locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let resolver = scratch.resolver("ws");
    let outcome = resolver.resolve("locked/file.txt");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The superuser can enter any directory, so there the path resolves.
    match outcome {
        Err(PathError::Io { .. }) => {}
        Ok(resolved) => assert!(
            resolved.exists(),
            "resolved only because nothing stops root"
        ),
        Err(error) => panic!("unexpected refusal: {error}"),
    }
}
