//! The no-follow open on a real file system: what it opens, what it
//! refuses, and that it neither follows a link nor waits on a FIFO that was
//! put at the path after the path was looked at.

use std::fs;
use std::io::Read;

use cortex_base::{
    AtomicWriteError, AtomicWriteOptions, FakeClock, OpenError, open_regular, write_atomic,
};

use crate::Scratch;

/// A write that must land exactly at its target: no flush, which these
/// tests do not depend on, and no link followed.
fn exactly_there() -> AtomicWriteOptions {
    AtomicWriteOptions {
        sync: false,
        follow_links: false,
        ..AtomicWriteOptions::default()
    }
}

#[test]
fn a_regular_file_is_opened_with_the_metadata_of_the_handle() {
    let scratch = Scratch::new("nofollow_regular");
    let path = scratch.file("notes.txt", "what the file holds");
    let (mut file, metadata) = open_regular(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.len(), 19);
    let mut content = String::new();
    file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "what the file holds");
}

#[test]
fn what_is_not_a_file_is_refused_by_name() {
    let scratch = Scratch::new("nofollow_refused");
    assert!(matches!(
        open_regular(&scratch.join("missing.txt")),
        Err(OpenError::NotFound)
    ));
    assert!(matches!(
        open_regular(&scratch.join("no-such-dir").join("missing.txt")),
        Err(OpenError::NotFound)
    ));
    let directory = scratch.dir("a-directory");
    assert!(matches!(
        open_regular(&directory),
        Err(OpenError::Directory)
    ));
}

#[test]
fn a_write_that_must_land_there_creates_and_replaces_like_any_other() {
    let scratch = Scratch::new("nofollow_write_plain");
    let clock = FakeClock::default();
    let target = scratch.join("notes.md");
    write_atomic(&target, b"first", &exactly_there(), &clock).unwrap();
    write_atomic(&target, b"second", &exactly_there(), &clock).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"second");
    assert_eq!(scratch.listing(""), ["notes.md"]);
    let directory = scratch.dir("a-directory");
    assert!(matches!(
        write_atomic(&directory, b"x", &exactly_there(), &clock),
        Err(AtomicWriteError::IsADirectory { .. })
    ));
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// A bound on a step that must not wait at all. It turns a mistake (an
    /// open that waits on the FIFO) into a failed test; no test that passes
    /// waits it out.
    const LIMIT: Duration = Duration::from_secs(5);

    #[test]
    fn a_link_is_never_followed_wherever_it_leads() {
        let scratch = Scratch::new("nofollow_link");
        let secret = scratch.file("outside/secret.txt", "not for this caller");
        let workspace = scratch.dir("ws");
        let to_file = workspace.join("to-file");
        let to_directory = workspace.join("to-directory");
        let to_nothing = workspace.join("to-nothing");
        symlink(&secret, &to_file).unwrap();
        symlink(scratch.join("outside"), &to_directory).unwrap();
        symlink(scratch.join("gone"), &to_nothing).unwrap();
        for link in [&to_file, &to_directory, &to_nothing] {
            assert!(
                matches!(open_regular(link), Err(OpenError::Link)),
                "{} was followed",
                link.display()
            );
        }
    }

    /// The window this module closes. The caller looks at a path and sees a
    /// regular file; before it opens the path, the file is swapped for a
    /// link to a file outside. The open must not hand back the outside
    /// file.
    #[test]
    fn a_file_swapped_for_a_link_after_the_look_is_not_followed() {
        let scratch = Scratch::new("nofollow_swap_link");
        let secret = scratch.file("outside/secret.txt", "not for this caller");
        let path = scratch.file("ws/notes.txt", "harmless");

        let looked = fs::symlink_metadata(&path).unwrap();
        assert!(looked.is_file(), "the look saw a regular file");
        fs::remove_file(&path).unwrap();
        symlink(&secret, &path).unwrap();

        assert!(matches!(open_regular(&path), Err(OpenError::Link)));
        // And a write that must land at the path does not go through the
        // link either: the outside file keeps its content.
        let clock = FakeClock::default();
        assert!(matches!(
            write_atomic(&path, b"overwritten", &exactly_there(), &clock),
            Err(AtomicWriteError::IsALink { .. })
        ));
        assert_eq!(fs::read(&secret).unwrap(), b"not for this caller");
        assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
        // The default still writes through, as it always has.
        let through = AtomicWriteOptions {
            sync: false,
            ..AtomicWriteOptions::default()
        };
        write_atomic(&path, b"through the link", &through, &clock).unwrap();
        assert_eq!(fs::read(&secret).unwrap(), b"through the link");
    }

    #[test]
    fn devices_and_sockets_are_not_files() {
        let scratch = Scratch::new("nofollow_special");
        assert!(matches!(
            open_regular(std::path::Path::new("/dev/null")),
            Err(OpenError::NotAFile)
        ));
        // A device that never ends is refused before a byte of it is read.
        assert!(matches!(
            open_regular(std::path::Path::new("/dev/zero")),
            Err(OpenError::NotAFile)
        ));
        let socket = scratch.join("agent.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(matches!(open_regular(&socket), Err(OpenError::NotAFile)));
    }

    fn make_fifo(path: &std::path::Path) {
        let made = std::process::Command::new("mkfifo")
            .arg(path)
            .output()
            .expect("run mkfifo");
        assert!(
            made.status.success(),
            "mkfifo failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
    }

    /// Opening a FIFO for reading waits until some program opens it for
    /// writing, which may be never. The file here is swapped for a FIFO
    /// after the look, and nothing ever writes to it. `mkfifo` is a
    /// process: hence the `sys_` name.
    #[test]
    fn sys_a_file_swapped_for_a_fifo_after_the_look_is_not_waited_on() {
        let scratch = Scratch::new("nofollow_swap_fifo");
        let path = scratch.file("ws/build.log", "harmless");

        let looked = fs::symlink_metadata(&path).unwrap();
        assert!(looked.is_file(), "the look saw a regular file");
        fs::remove_file(&path).unwrap();
        make_fifo(&path);

        // The open runs on a thread of its own, so that an open that waits
        // is a failed test and not a hung one.
        let (done_tx, done_rx) = mpsc::channel();
        let opened = path.clone();
        std::thread::spawn(move || {
            let outcome = open_regular(&opened).map(|_| ());
            let _ = done_tx.send(outcome);
        });
        let outcome = done_rx
            .recv_timeout(LIMIT)
            .expect("the open waited on a FIFO nobody writes to");
        assert!(
            matches!(outcome, Err(OpenError::NotAFile)),
            "a FIFO is not a file: {outcome:?}"
        );

        // A write that must land at the path refuses the FIFO, and does not
        // wait on it either.
        let (done_tx, done_rx) = mpsc::channel();
        let target = path.clone();
        std::thread::spawn(move || {
            let outcome = write_atomic(&target, b"x", &exactly_there(), &FakeClock::default());
            let _ = done_tx.send(outcome);
        });
        let outcome = done_rx
            .recv_timeout(LIMIT)
            .expect("the write waited on a FIFO nobody reads");
        assert!(
            matches!(outcome, Err(AtomicWriteError::NotAFile { .. })),
            "{outcome:?}"
        );
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::fs::{OpenOptionsExt, symlink_dir, symlink_file};
    use std::path::Path;

    use super::*;

    /// Whether this is the CI job, where every Windows case must really run.
    fn in_ci() -> bool {
        std::env::var_os("GITHUB_ACTIONS").is_some()
    }

    /// Making a symbolic link needs a privilege an ordinary account lacks
    /// unless Developer Mode is on. The CI runner has it, and there the
    /// link must be made.
    fn made_or_skip(made: std::io::Result<()>) -> bool {
        match made {
            Ok(()) => true,
            Err(error) if !in_ci() => {
                println!("skipped: cannot make a symbolic link here ({error})");
                false
            }
            Err(error) => panic!("the CI runner cannot make a symbolic link: {error}"),
        }
    }

    /// A real symbolic link, to a file and to a directory: the open asks
    /// for the reparse point itself, reads its tag, and refuses it.
    #[test]
    fn a_symbolic_link_is_never_followed() {
        let scratch = Scratch::new("nofollow_win_symlink");
        let secret = scratch.file("outside/secret.txt", "not for this caller");
        let workspace = scratch.dir("ws");
        let to_file = workspace.join("to-file.txt");
        let to_directory = workspace.join("to-directory");
        if !made_or_skip(symlink_file(&secret, &to_file))
            || !made_or_skip(symlink_dir(scratch.join("outside"), &to_directory))
        {
            return;
        }
        for link in [&to_file, &to_directory] {
            assert!(
                matches!(open_regular(link), Err(OpenError::Link)),
                "{} was followed",
                link.display()
            );
        }
        // A write that must land at the link's name does not go through it.
        let clock = FakeClock::default();
        assert!(matches!(
            write_atomic(&to_file, b"overwritten", &exactly_there(), &clock),
            Err(AtomicWriteError::IsALink { .. })
        ));
        assert_eq!(fs::read(&secret).unwrap(), b"not for this caller");
    }

    /// The window this module closes, on Windows: a regular file at the
    /// look, a link to a file outside at the open.
    #[test]
    fn a_file_swapped_for_a_link_after_the_look_is_not_followed() {
        let scratch = Scratch::new("nofollow_win_swap");
        let secret = scratch.file("outside/secret.txt", "not for this caller");
        let path = scratch.file("ws/notes.txt", "harmless");
        assert!(fs::symlink_metadata(&path).unwrap().is_file());
        fs::remove_file(&path).unwrap();
        if !made_or_skip(symlink_file(&secret, &path)) {
            return;
        }
        assert!(matches!(open_regular(&path), Err(OpenError::Link)));
    }

    /// A junction is the directory link any account can make. It is made
    /// with `mklink`, which is a process: hence the `sys_` name.
    #[test]
    fn sys_a_junction_is_never_followed() {
        let scratch = Scratch::new("nofollow_win_junction");
        scratch.file("outside/secret.txt", "s");
        let link = scratch.dir("ws").join("vendor");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(scratch.join("outside"))
            .output()
            .expect("run mklink");
        assert!(
            made.status.success(),
            "mklink failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        assert!(matches!(open_regular(&link), Err(OpenError::Link)));
        // The file behind the junction is an ordinary file: only the last
        // name of a path is guarded.
        assert!(open_regular(&link.join("secret.txt")).is_ok());
    }

    /// A file another program holds with no sharing cannot be opened, and
    /// the error says that this is why.
    #[test]
    fn a_file_held_without_sharing_is_reported_as_in_use() {
        let scratch = Scratch::new("nofollow_win_locked");
        let path = scratch.file("report.docx", "the original");
        let holder = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .expect("open the file with no sharing");
        let refused = open_regular(Path::new(&path)).expect_err("the file is held");
        assert!(refused.is_in_use(), "{refused:?}");
        drop(holder);
        assert!(open_regular(&path).is_ok(), "let go, it opens");
    }
}
