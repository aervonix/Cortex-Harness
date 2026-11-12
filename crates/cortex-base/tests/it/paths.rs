//! Resolving paths against a real file system: what holds on every OS.

use cortex_base::{AbsPath, PathError, PathOptions, PathResolver, PathStyle};

use crate::Scratch;

#[test]
fn a_path_inside_the_workspace_resolves_to_its_real_location() {
    let scratch = Scratch::new("resolve_inside");
    scratch.file("ws/src/main.rs", "fn main() {}");
    let resolver = scratch.resolver("ws");
    assert_eq!(resolver.root(), &scratch.real("ws"));

    let resolved = resolver.resolve("src/main.rs").unwrap();
    assert!(resolved.exists());
    assert!(resolved.is_inside_workspace());
    assert_eq!(resolved.relative().unwrap().as_str(), "src/main.rs");
    assert_eq!(resolved.display(), "src/main.rs");
    assert_eq!(resolved.abs(), &scratch.real("ws/src/main.rs"));
    assert_eq!(
        std::fs::read_to_string(resolved.to_fs_path()).unwrap(),
        "fn main() {}",
        "the file-system form opens the file"
    );

    // Every spelling of the same file resolves to the same thing.
    for spelling in [
        "./src/main.rs",
        "src//main.rs",
        "src/./main.rs",
        "src/../src/main.rs",
    ] {
        assert_eq!(
            resolver.resolve(spelling).unwrap(),
            resolved,
            "{spelling:?}"
        );
    }
    assert!(!resolved.names_a_directory());
    // And so does its absolute path.
    let absolute = scratch.join("ws/src/main.rs");
    assert_eq!(
        resolver.resolve(absolute.to_str().unwrap()).unwrap(),
        resolved
    );
}

#[test]
fn the_workspace_root_resolves_to_itself() {
    let scratch = Scratch::new("resolve_root");
    let resolver = scratch.resolver("ws");
    for spelling in [".", "./", "a/..", "./."] {
        let resolved = resolver.resolve(spelling).unwrap();
        assert_eq!(resolved.abs(), resolver.root(), "{spelling:?}");
        assert!(resolved.is_inside_workspace());
        assert!(resolved.relative().unwrap().is_empty());
        assert_eq!(resolved.display(), ".");
        assert!(resolved.exists());
    }
}

#[test]
fn a_file_that_does_not_exist_yet_resolves_under_its_real_directory() {
    let scratch = Scratch::new("resolve_new");
    scratch.dir("ws/src");
    let resolver = scratch.resolver("ws");

    let resolved = resolver.resolve("src/new.rs").unwrap();
    assert!(!resolved.exists());
    assert!(resolved.is_inside_workspace());
    assert_eq!(resolved.display(), "src/new.rs");
    assert_eq!(
        resolved.abs(),
        &scratch.real("ws/src").join("new.rs").unwrap()
    );

    // Several levels that do not exist, with steps back and forth in them.
    let resolved = resolver.resolve("src/a/b/../c/./new.rs").unwrap();
    assert!(!resolved.exists());
    assert_eq!(resolved.display(), "src/a/c/new.rs");
    let resolved = resolver.resolve("missing/../src/x/../../top.rs").unwrap();
    assert_eq!(resolved.display(), "top.rs");
    assert!(!resolved.exists());
    // Stepping back out of the part that does not exist lands on what does.
    let resolved = resolver.resolve("missing/deeper/../../src").unwrap();
    assert_eq!(resolved.display(), "src");
}

#[test]
fn a_path_outside_the_workspace_resolves_and_says_so() {
    let scratch = Scratch::new("resolve_outside");
    scratch.file("elsewhere/secret.txt", "s");
    let resolver = scratch.resolver("ws");

    for spelling in ["../elsewhere/secret.txt", "a/../../elsewhere/secret.txt"] {
        let resolved = resolver.resolve(spelling).unwrap();
        assert!(!resolved.is_inside_workspace(), "{spelling:?}");
        assert_eq!(resolved.relative(), None);
        assert!(resolved.exists());
        assert_eq!(resolved.abs(), &scratch.real("elsewhere/secret.txt"));
        assert_eq!(resolved.display(), resolved.abs().as_str(), "shown in full");
        assert!(
            !resolved.display().contains('\\'),
            "shown with forward slashes"
        );
    }

    let absolute = scratch.join("elsewhere/secret.txt");
    let resolved = resolver.resolve(absolute.to_str().unwrap()).unwrap();
    assert!(!resolved.is_inside_workspace());

    // A sibling whose name starts like the workspace's is outside it.
    scratch.file("ws2/file.txt", "x");
    let resolved = resolver.resolve("../ws2/file.txt").unwrap();
    assert!(!resolved.is_inside_workspace());
    // Climbing above the file system's root stays at the root.
    let resolved = resolver.resolve(&"../".repeat(64)).unwrap();
    assert!(resolved.abs().is_root());
    assert!(!resolved.is_inside_workspace());
}

#[test]
fn a_tilde_is_the_home_directory_when_one_is_known() {
    let scratch = Scratch::new("resolve_home");
    scratch.file("home/notes/todo.md", "x");
    let home = scratch.real("home");
    let resolver = scratch.resolver_with("ws", PathOptions::new().home(home.clone()));

    let resolved = resolver.resolve("~/notes/todo.md").unwrap();
    assert_eq!(resolved.abs(), &scratch.real("home/notes/todo.md"));
    assert!(!resolved.is_inside_workspace());
    assert_eq!(resolver.resolve("~").unwrap().abs(), &home);
    assert_eq!(
        resolver.resolve("~/new.txt").unwrap().abs(),
        &home.join("new.txt").unwrap()
    );

    // Only a leading `~` on its own is the home directory.
    assert_eq!(resolver.resolve("~notes").unwrap().display(), "~notes");
    assert_eq!(resolver.resolve("src/~").unwrap().display(), "src/~");
    assert_eq!(resolver.resolve("./~").unwrap().display(), "~");

    let no_home = scratch.resolver("ws");
    assert!(matches!(
        no_home.resolve("~/notes/todo.md"),
        Err(PathError::NoHome { .. })
    ));
    assert!(matches!(
        no_home.resolve("~"),
        Err(PathError::NoHome { .. })
    ));
}

#[test]
fn the_home_directory_can_come_from_the_environment() {
    // Whatever the environment says, the option is usable: a resolver is
    // built, and `~` either resolves to an absolute path or is refused.
    let scratch = Scratch::new("resolve_home_env");
    let resolver = scratch.resolver_with("ws", PathOptions::new().home_from_env());
    match resolver.resolve("~") {
        Ok(resolved) => assert!(!resolved.abs().as_str().is_empty()),
        Err(
            PathError::NoHome { .. } | PathError::Io { .. } | PathError::ShareNotAllowed { .. },
        ) => {}
        Err(error) => panic!("unexpected refusal: {error}"),
    }
}

#[test]
fn bad_input_is_refused_before_the_file_system_is_asked() {
    let scratch = Scratch::new("resolve_bad");
    let resolver = scratch.resolver("ws");
    assert!(matches!(resolver.resolve(""), Err(PathError::Empty)));
    assert!(matches!(
        resolver.resolve("src/ma\0in.rs"),
        Err(PathError::ContainsNul { .. })
    ));
}

#[test]
fn a_path_through_a_file_is_refused() {
    let scratch = Scratch::new("resolve_through_file");
    scratch.file("ws/file.txt", "x");
    let resolver = scratch.resolver("ws");
    for spelling in ["file.txt/inside", "file.txt/a/b"] {
        assert!(
            matches!(resolver.resolve(spelling), Err(PathError::Io { .. })),
            "{spelling:?} leads through a file"
        );
    }
}

#[test]
fn a_trailing_separator_names_a_directory() {
    let scratch = Scratch::new("resolve_trailing_separator");
    scratch.file("ws/src/main.rs", "x");
    let resolver = scratch.resolver("ws");

    // On an existing directory it changes nothing but the flag.
    let directory = resolver.resolve("src/").unwrap();
    assert!(directory.names_a_directory());
    assert_eq!(directory.abs(), resolver.resolve("src").unwrap().abs());
    assert!(!resolver.resolve("src").unwrap().names_a_directory());

    // On an existing file it is refused, as the OS would refuse it.
    let error = resolver.resolve("src/main.rs/").unwrap_err();
    assert!(matches!(error, PathError::Io { .. }), "{error}");

    // On something that does not exist yet, the caller is told, so that a
    // write to `out/` does not create a file named `out`.
    let new = resolver.resolve("out/").unwrap();
    assert!(!new.exists());
    assert!(new.names_a_directory());
    assert_eq!(new.display(), "out");
    assert!(!resolver.resolve("out").unwrap().names_a_directory());
}

#[test]
fn the_workspace_must_be_an_existing_directory() {
    let scratch = Scratch::new("resolver_new");
    let file = scratch.file("file.txt", "x");
    assert!(matches!(
        PathResolver::new(&file, PathOptions::new()),
        Err(PathError::NotADirectory { .. })
    ));
    assert!(matches!(
        PathResolver::new(&scratch.join("missing"), PathOptions::new()),
        Err(PathError::Io { .. })
    ));
    assert!(matches!(
        PathResolver::new(std::path::Path::new("relative/dir"), PathOptions::new()),
        Err(PathError::NotAbsolute { .. })
    ));
    assert!(matches!(
        PathResolver::new(std::path::Path::new(""), PathOptions::new()),
        Err(PathError::Empty)
    ));
}

#[test]
fn a_resolved_path_round_trips_through_the_text_layer() {
    let scratch = Scratch::new("resolve_round_trip");
    scratch.file("ws/a/b/c.txt", "x");
    let resolver = scratch.resolver("ws");
    let resolved = resolver.resolve("a/b/c.txt").unwrap();
    let abs = resolved.abs();

    assert_eq!(abs.style(), PathStyle::NATIVE);
    assert_eq!(
        &AbsPath::parse(abs.as_str(), PathStyle::NATIVE).unwrap(),
        abs
    );
    assert_eq!(
        &AbsPath::parse(&abs.to_native_string(), PathStyle::NATIVE).unwrap(),
        abs
    );
    assert_eq!(&AbsPath::from_os(&abs.to_fs_path()).unwrap(), abs);
    assert!(abs.starts_with(resolver.root()));
    assert_eq!(
        &resolver
            .root()
            .join(resolved.relative().unwrap().as_str())
            .unwrap(),
        abs
    );
    // Resolving what was resolved changes nothing.
    assert_eq!(resolver.resolve(abs.as_str()).unwrap(), resolved);
    assert_eq!(resolver.resolve(&abs.to_native_string()).unwrap(), resolved);

    // A directory listing's entries become paths one at a time.
    let directory = resolver.resolve("a/b").unwrap();
    for entry in std::fs::read_dir(directory.to_fs_path()).unwrap() {
        let child = directory.abs().child(&entry.unwrap().file_name()).unwrap();
        assert_eq!(&child, abs);
    }
}
