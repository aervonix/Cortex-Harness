//! Owner-only files and directories on a real file system
//! (`docs/decisions/0029-owner-only-files-on-windows.md`).
//!
//! The Windows tests read the access list back from the file system and
//! find the current user alone, with inheritance off: the "Permissions on
//! files" row of the Windows coverage in `docs/testing-and-ci.md`. One of
//! them asks `icacls`, a program that shares no code with the module, for a
//! second opinion.

use std::fs;
use std::io::{self, Write};

use cortex_base::private::{create_dir_all, create_new, is_private};
use cortex_base::{AtomicWriteOptions, FakeClock, write_atomic};

use crate::Scratch;

fn owner_only() -> AtomicWriteOptions {
    AtomicWriteOptions {
        sync: false,
        owner_only: true,
        ..AtomicWriteOptions::default()
    }
}

#[test]
fn a_new_file_is_private_and_its_owner_can_still_use_it() {
    let scratch = Scratch::new("private_file");
    let path = scratch.join("session.jsonl");

    let mut file = create_new(&path).unwrap();
    assert!(is_private(&path).unwrap(), "private before any content");
    file.write_all(b"first line\n").unwrap();
    drop(file);

    // The owner reads it, appends to it and replaces it like any file.
    assert_eq!(fs::read(&path).unwrap(), b"first line\n");
    let mut again = fs::OpenOptions::new().append(true).open(&path).unwrap();
    again.write_all(b"second line\n").unwrap();
    drop(again);
    assert_eq!(fs::read(&path).unwrap(), b"first line\nsecond line\n");
    assert!(is_private(&path).unwrap(), "still private after an append");

    fs::remove_file(&path).unwrap();
}

#[test]
fn a_name_that_is_taken_is_refused_and_left_alone() {
    let scratch = Scratch::new("private_taken");
    let path = scratch.file("taken.txt", "keep me");

    let error = create_new(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");

    // A directory under the name is refused too. (Windows reports it as a
    // refusal of access, so only the fact of the error is common.)
    let directory = scratch.dir("a-directory");
    assert!(create_new(&directory).is_err());
    assert!(directory.is_dir());

    // The directory must exist: nothing is created on the way.
    let error = create_new(&scratch.join("missing/file.txt")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    assert!(!scratch.join("missing").exists());
}

#[test]
fn missing_directories_are_created_private_and_existing_ones_are_left_alone() {
    let scratch = Scratch::new("private_dirs");
    let existing = scratch.dir("existing");
    let before = is_private(&existing).unwrap();

    let deepest = existing.join("state").join("projects").join("abc");
    create_dir_all(&deepest).unwrap();

    for made in [
        existing.join("state"),
        existing.join("state").join("projects"),
        deepest.clone(),
    ] {
        assert!(made.is_dir(), "{}", made.display());
        assert!(is_private(&made).unwrap(), "{}", made.display());
    }
    assert_eq!(
        is_private(&existing).unwrap(),
        before,
        "a directory that was there keeps what it had"
    );

    // Asking again changes nothing and is not an error.
    create_dir_all(&deepest).unwrap();
    create_dir_all(&existing).unwrap();
    assert_eq!(is_private(&existing).unwrap(), before);

    // A file inside a private directory can be made and used.
    let file = deepest.join("knowledge.toml");
    write_atomic(&file, b"x = 1\n", &owner_only(), &FakeClock::default()).unwrap();
    assert!(is_private(&file).unwrap());
    assert_eq!(fs::read(&file).unwrap(), b"x = 1\n");
    assert_eq!(
        scratch.listing("existing/state/projects/abc"),
        ["knowledge.toml"]
    );
}

#[test]
fn a_file_on_the_way_is_an_error() {
    let scratch = Scratch::new("private_dirs_blocked");
    let file = scratch.file("not-a-directory", "x");
    assert!(create_dir_all(&file.join("below")).is_err());
    assert!(create_dir_all(&file).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}

#[test]
fn an_owner_only_write_leaves_a_private_file_new_or_not() {
    let scratch = Scratch::new("private_atomic");
    let clock = FakeClock::default();

    let new = scratch.join("credentials.toml");
    write_atomic(&new, b"one", &owner_only(), &clock).unwrap();
    assert!(is_private(&new).unwrap());

    // Written again, with the option and without: it stays private on
    // Linux, where the mode of a replaced file is kept. On Windows a write
    // without the option gives the file its directory's rules, which is why
    // every write of a private file passes the option.
    write_atomic(&new, b"two", &owner_only(), &clock).unwrap();
    assert!(is_private(&new).unwrap());
    assert_eq!(fs::read(&new).unwrap(), b"two");

    assert_eq!(scratch.listing(""), ["credentials.toml"]);
    assert!(clock.sleeps().is_empty());
}

#[test]
fn asking_about_a_path_that_is_not_there_is_an_error() {
    let scratch = Scratch::new("private_missing");
    let error = is_private(&scratch.join("nothing-here")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn mode(path: &std::path::Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn the_modes_are_0600_and_0700() {
        let scratch = Scratch::new("private_unix_modes");
        let file = scratch.join("file");
        drop(create_new(&file).unwrap());
        assert_eq!(mode(&file), 0o600);

        let directory = scratch.join("a/b");
        create_dir_all(&directory).unwrap();
        assert_eq!(mode(&scratch.join("a")), 0o700);
        assert_eq!(mode(&directory), 0o700);
    }

    #[test]
    fn any_group_or_other_bit_means_not_private() {
        let scratch = Scratch::new("private_unix_bits");
        let file = scratch.file("file", "x");
        for (bits, private) in [
            (0o600, true),
            (0o400, true),
            (0o700, true),
            (0o640, false),
            (0o604, false),
            (0o610, false),
            (0o601, false),
            (0o644, false),
            (0o666, false),
        ] {
            fs::set_permissions(&file, fs::Permissions::from_mode(bits)).unwrap();
            assert_eq!(is_private(&file).unwrap(), private, "mode {bits:o}");
        }
    }

    #[test]
    fn a_link_under_the_name_is_not_followed() {
        let scratch = Scratch::new("private_unix_link");
        let link = scratch.join("credentials.toml");
        symlink("elsewhere.toml", &link).unwrap();

        let error = create_new(&link).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
        assert!(
            !scratch.join("elsewhere.toml").exists(),
            "nothing was created where the link points"
        );
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::fs::symlink_file;
    use std::path::Path;

    use cortex_base::private::{AccessKind, AccessList, FULL_CONTROL, access_list};

    use super::*;

    /// Whether this is the CI job, where every Windows case must really run.
    fn in_ci() -> bool {
        std::env::var_os("GITHUB_ACTIONS").is_some()
    }

    /// The whole of what "owner-only" means, checked field by field on the
    /// list as the file system reports it.
    fn assert_only_the_current_user(path: &Path) -> AccessList {
        let list = access_list(path).unwrap();
        let shown = format!("{}: {list:?}", path.display());
        assert!(!list.inherits, "inheritance is off: {shown}");
        assert!(!list.unrestricted, "there is an access list: {shown}");
        assert!(list.owned_by_current_user, "owned by the user: {shown}");
        assert_eq!(list.entries.len(), 1, "one entry: {shown}");
        let entry = &list.entries[0];
        assert_eq!(entry.kind, AccessKind::Allow, "{shown}");
        assert!(entry.current_user, "the entry is the user's: {shown}");
        assert_eq!(entry.trustee, list.owner, "{shown}");
        assert_eq!(entry.rights, FULL_CONTROL, "{shown}");
        assert!(!entry.inherited, "set on the object itself: {shown}");
        assert!(list.is_owner_only(), "{shown}");
        assert!(is_private(path).unwrap(), "{shown}");
        list
    }

    #[test]
    fn a_new_file_has_only_the_current_user_on_its_access_list() {
        let scratch = Scratch::new("private_win_file");

        // The scratch directory is an ordinary one: it takes its rules from
        // the folder above, and a file made in it the ordinary way does too.
        let ordinary = scratch.file("ordinary.txt", "x");
        let inherited = access_list(&ordinary).unwrap();
        assert!(inherited.inherits, "{inherited:?}");
        assert!(!inherited.is_owner_only(), "{inherited:?}");
        assert!(!is_private(&ordinary).unwrap());
        assert!(!is_private(ordinary.parent().unwrap()).unwrap());

        // Read back while the file is still open and empty: the list was
        // there from the moment the file was.
        let path = scratch.join("private.txt");
        let mut file = create_new(&path).unwrap();
        assert_only_the_current_user(&path);
        file.write_all(b"secret").unwrap();
        drop(file);
        assert_only_the_current_user(&path);
        assert_eq!(fs::read(&path).unwrap(), b"secret");
    }

    #[test]
    fn an_owner_only_write_gives_the_file_the_list_new_or_existing() {
        let scratch = Scratch::new("private_win_atomic");
        let clock = FakeClock::default();

        let new = scratch.join("credentials.toml");
        write_atomic(&new, b"one", &owner_only(), &clock).unwrap();
        assert_only_the_current_user(&new);
        write_atomic(&new, b"two", &owner_only(), &clock).unwrap();
        assert_only_the_current_user(&new);
        assert_eq!(fs::read(&new).unwrap(), b"two");

        // A file made by hand, open to whoever the folder is open to, is
        // private once it has been written with the option.
        let by_hand = scratch.file("by-hand.toml", "# made by hand\n");
        assert!(!is_private(&by_hand).unwrap());
        write_atomic(&by_hand, b"key", &owner_only(), &clock).unwrap();
        assert_only_the_current_user(&by_hand);
        assert_eq!(fs::read(&by_hand).unwrap(), b"key");

        // Without the option the new file takes the folder's rules again.
        // That is the documented behaviour, and the reason every write of a
        // private file passes the option.
        let plain = AtomicWriteOptions {
            sync: false,
            ..AtomicWriteOptions::default()
        };
        write_atomic(&by_hand, b"key", &plain, &clock).unwrap();
        assert!(!is_private(&by_hand).unwrap());

        assert_eq!(scratch.listing(""), ["by-hand.toml", "credentials.toml"]);
    }

    #[test]
    fn a_new_directory_is_private_and_hands_its_list_down() {
        let scratch = Scratch::new("private_win_dir");
        let outer = scratch.join("state");
        let inner = outer.join("projects");
        create_dir_all(&inner).unwrap();
        assert_only_the_current_user(&outer);
        assert_only_the_current_user(&inner);

        // A file made the ordinary way inside it gets an inherited copy of
        // the entry: the user alone can open it. It is still not "private"
        // in this module's sense, because it follows its folder.
        let ordinary = inner.join("ordinary.txt");
        fs::write(&ordinary, "x").unwrap();
        let list = access_list(&ordinary).unwrap();
        assert!(list.inherits, "{list:?}");
        assert!(!list.entries.is_empty(), "{list:?}");
        assert!(
            list.entries
                .iter()
                .all(|entry| entry.kind == AccessKind::Allow
                    && entry.current_user
                    && entry.inherited),
            "{list:?}"
        );
        assert!(!is_private(&ordinary).unwrap());

        // And a file made through the module is private in the full sense.
        let private = inner.join("private.txt");
        drop(create_new(&private).unwrap());
        assert_only_the_current_user(&private);
    }

    #[test]
    fn a_path_longer_than_the_old_limit_works() {
        let scratch = Scratch::new("private_win_long");
        let mut directory = scratch.join("long");
        for _ in 0..8 {
            directory.push("d".repeat(40));
        }
        create_dir_all(&directory).unwrap();
        let file = directory.join("f".repeat(40));
        assert!(
            file.as_os_str().len() > 300,
            "the path is {} long",
            file.as_os_str().len()
        );

        drop(create_new(&file).unwrap());
        assert_only_the_current_user(&file);
        assert_only_the_current_user(&directory);

        let written = directory.join("written.toml");
        write_atomic(&written, b"x", &owner_only(), &FakeClock::default()).unwrap();
        assert_only_the_current_user(&written);
    }

    #[test]
    fn a_link_under_the_name_is_not_followed() {
        let scratch = Scratch::new("private_win_link");
        let link = scratch.join("credentials.toml");
        // A file link needs a privilege an ordinary account lacks unless
        // Developer Mode is on. The CI runner has it.
        match symlink_file(scratch.join("elsewhere.toml"), &link) {
            Ok(()) => {}
            Err(error) if !in_ci() => {
                println!("skipped: cannot make a file link here ({error})");
                return;
            }
            Err(error) => panic!("the CI runner cannot make a file link: {error}"),
        }

        let error = create_new(&link).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
        assert!(
            !scratch.join("elsewhere.toml").exists(),
            "nothing was created where the link points"
        );
    }

    /// A second opinion from a program that shares no code with the module.
    /// `icacls` prints one line per entry, `NAME:(rights)`, with `(I)` on an
    /// inherited one. It is a process: hence the `sys_` name.
    #[test]
    fn sys_icacls_agrees_that_only_the_current_user_is_listed() {
        let scratch = Scratch::new("private_win_icacls");
        let file = scratch.join("private.txt");
        drop(create_new(&file).unwrap());
        let directory = scratch.join("private-dir");
        create_dir_all(&directory).unwrap();
        let ordinary = scratch.file("ordinary.txt", "x");

        let entries_of = |path: &Path| -> Vec<String> {
            let output = std::process::Command::new("icacls")
                .arg(path)
                .output()
                .expect("run icacls");
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            assert!(output.status.success(), "icacls failed: {text}");
            // The first entry follows the path on the first line.
            let shown = path.display().to_string();
            text.lines()
                .map(|line| line.strip_prefix(shown.as_str()).unwrap_or(line).trim())
                .filter(|line| line.contains(":("))
                .map(str::to_owned)
                .collect()
        };
        let user = std::env::var("USERNAME").expect("the runner sets USERNAME");

        let listed = entries_of(&file);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(
            listed[0].ends_with(&format!("\\{user}:(F)")),
            "full control for {user}, not inherited: {listed:?}"
        );

        let listed = entries_of(&directory);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(
            listed[0].ends_with(&format!("\\{user}:(OI)(CI)(F)")),
            "full control for {user}, handed down, not inherited: {listed:?}"
        );

        // The control: an ordinary file here has inherited entries, so the
        // parsing above does see them when they are there.
        let listed = entries_of(&ordinary);
        assert!(
            listed.iter().any(|entry| entry.contains("(I)")),
            "{listed:?}"
        );
    }
}
