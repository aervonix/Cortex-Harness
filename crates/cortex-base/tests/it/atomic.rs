//! Atomic writes on a real file system.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cortex_base::{AtomicWriteError, AtomicWriteOptions, FakeClock, write_atomic};

use crate::Scratch;

/// The default options, without the flush to disk. What these tests check
/// does not depend on it, and a flush is the one step whose time belongs to
/// the disk: it took tens of milliseconds a call on the build machine, and
/// once several seconds. `a_synced_write_gives_the_same_file` covers it.
fn options() -> AtomicWriteOptions {
    AtomicWriteOptions {
        sync: false,
        ..AtomicWriteOptions::default()
    }
}

#[test]
fn a_new_file_is_created_and_an_old_one_replaced() {
    let scratch = Scratch::new("atomic_basic");
    let clock = FakeClock::default();
    let target = scratch.join("notes.md");

    write_atomic(&target, b"first", &options(), &clock).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"first");

    // Bytes are written as given: no line-ending or encoding changes.
    let exact = b"line one\r\nline two\n\x00\xff\xfe binary\r";
    write_atomic(&target, exact, &options(), &clock).unwrap();
    assert_eq!(fs::read(&target).unwrap(), exact);

    write_atomic(&target, b"", &options(), &clock).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"");

    let large = vec![b'x'; 1024 * 1024];
    write_atomic(&target, &large, &options(), &clock).unwrap();
    assert_eq!(fs::read(&target).unwrap(), large);

    assert_eq!(
        scratch.listing(""),
        ["notes.md"],
        "no temporary file is left"
    );
    assert!(clock.sleeps().is_empty(), "nothing was waited for");
}

#[test]
fn a_synced_write_gives_the_same_file() {
    let scratch = Scratch::new("atomic_sync");
    let target = scratch.join("state.bin");
    let synced = AtomicWriteOptions::default();
    assert!(synced.sync, "flushing is the default");
    assert!(!synced.owner_only);
    assert_eq!(synced.retry_waits.len(), 6);
    write_atomic(&target, b"one", &synced, &FakeClock::default()).unwrap();
    write_atomic(&target, b"two", &synced, &FakeClock::default()).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"two");
    assert_eq!(scratch.listing(""), ["state.bin"]);
}

#[test]
fn a_failed_write_changes_nothing_and_leaves_nothing() {
    let scratch = Scratch::new("atomic_failures");
    let clock = FakeClock::default();

    // The target is a directory.
    scratch.dir("target-dir");
    let error = write_atomic(&scratch.join("target-dir"), b"x", &options(), &clock).unwrap_err();
    assert!(
        matches!(error, AtomicWriteError::IsADirectory { .. }),
        "{error}"
    );
    assert!(scratch.listing("target-dir").is_empty());

    // The directory does not exist. It is not created.
    let error = write_atomic(
        &scratch.join("missing/dir/file.txt"),
        b"x",
        &options(),
        &clock,
    )
    .unwrap_err();
    assert!(
        matches!(&error, AtomicWriteError::Io { step, .. } if step.contains("temporary")),
        "{error}"
    );
    assert!(!scratch.join("missing").exists());
    assert!(error.to_string().contains("file.txt"), "{error}");

    assert_eq!(
        scratch.listing(""),
        ["target-dir"],
        "no temporary file is left"
    );
    assert!(clock.sleeps().is_empty());
}

#[test]
fn a_read_only_file_is_refused_and_keeps_its_content() {
    let scratch = Scratch::new("atomic_read_only");
    let target = scratch.file("protected.txt", "keep me");
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions).unwrap();

    let error =
        write_atomic(&target, b"overwritten", &options(), &FakeClock::default()).unwrap_err();
    assert!(
        matches!(error, AtomicWriteError::ReadOnly { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("read-only"), "{error}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep me");
    assert!(fs::metadata(&target).unwrap().permissions().readonly());
    assert_eq!(scratch.listing(""), ["protected.txt"]);
}

#[test]
fn a_reader_sees_the_old_content_or_the_new_and_never_a_mixture() {
    let scratch = Scratch::new("atomic_readers");
    let target = scratch.join("shared.txt");
    let old = vec![b'a'; 256 * 1024];
    let new = vec![b'b'; 300 * 1024];
    fs::write(&target, &old).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (target, stop, old, new) =
            (target.clone(), Arc::clone(&stop), old.clone(), new.clone());
        std::thread::spawn(move || {
            let mut reads = 0u32;
            let mut torn = 0u32;
            while !stop.load(Ordering::Relaxed) || reads == 0 {
                // A read can fail outright while the file is being swapped
                // on Windows. What it must never do is succeed with part of
                // one content or a mixture of the two.
                if let Ok(seen) = fs::read(&target) {
                    reads += 1;
                    if seen != old && seen != new {
                        torn += 1;
                    }
                }
            }
            (reads, torn)
        })
    };

    let mut replaced = 0;
    for round in 0..200 {
        let contents = if round % 2 == 0 { &new } else { &old };
        // On Windows a reader that has the file open at the wrong moment
        // can make one replace fail as "in use": the retries wait on the
        // fake clock, so they take no time and the reader may still be
        // there. The file is then unchanged, which is the property under
        // test.
        match write_atomic(&target, contents, &options(), &FakeClock::default()) {
            Ok(()) => replaced += 1,
            Err(AtomicWriteError::InUse { .. }) if cfg!(windows) => {}
            Err(error) => panic!("round {round}: {error}"),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let (reads, torn) = reader.join().expect("the reader finishes");

    assert!(replaced > 0, "no replace succeeded");
    assert!(reads > 0, "the reader never read");
    assert_eq!(
        torn, 0,
        "{torn} of {reads} reads saw a file that was neither version"
    );
    let last = fs::read(&target).unwrap();
    assert!(last == old || last == new);
    assert_eq!(scratch.listing(""), ["shared.txt"]);
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use super::*;

    fn mode(path: &std::path::Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn the_mode_of_a_replaced_file_is_kept() {
        let scratch = Scratch::new("atomic_unix_mode");
        let clock = FakeClock::default();
        for wanted in [0o640, 0o755, 0o600, 0o664, 0o4750] {
            let target = scratch.file("file", "old");
            fs::set_permissions(&target, fs::Permissions::from_mode(wanted)).unwrap();
            write_atomic(&target, b"new", &options(), &clock).unwrap();
            assert_eq!(mode(&target), wanted, "mode {wanted:o}");
            assert_eq!(fs::read(&target).unwrap(), b"new");
            fs::remove_file(&target).unwrap();
        }
    }

    #[test]
    fn a_new_file_gets_the_usual_mode_or_an_owner_only_one() {
        let scratch = Scratch::new("atomic_unix_new_mode");
        let clock = FakeClock::default();

        // What the process's umask gives any new file.
        let ordinary = scratch.file("ordinary", "x");
        let usual = mode(&ordinary);

        let target = scratch.join("public.txt");
        write_atomic(&target, b"x", &options(), &clock).unwrap();
        assert_eq!(mode(&target), usual);

        let private = AtomicWriteOptions {
            owner_only: true,
            ..options()
        };
        let target = scratch.join("credentials");
        write_atomic(&target, b"key", &private, &clock).unwrap();
        assert_eq!(mode(&target), 0o600);

        // An existing file keeps its mode, whatever the option says.
        write_atomic(&ordinary, b"y", &private, &clock).unwrap();
        assert_eq!(mode(&ordinary), usual);
        write_atomic(&target, b"key2", &options(), &clock).unwrap();
        assert_eq!(mode(&target), 0o600);
    }

    #[test]
    fn a_link_is_written_through_and_stays_a_link() {
        let scratch = Scratch::new("atomic_unix_link");
        let real = scratch.file("real/config.toml", "old");
        let link = scratch.join("config.toml");
        symlink("real/config.toml", &link).unwrap();

        write_atomic(&link, b"new", &options(), &FakeClock::default()).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
        assert_eq!(
            scratch.listing("real"),
            ["config.toml"],
            "the temporary file was beside the real one"
        );
        assert_eq!(scratch.listing(""), ["config.toml", "real"]);

        // A link to nothing is an error, and creates nothing.
        let dangling = scratch.join("dangling");
        symlink("real/missing.toml", &dangling).unwrap();
        let error = write_atomic(&dangling, b"x", &options(), &FakeClock::default()).unwrap_err();
        assert!(matches!(error, AtomicWriteError::Io { .. }), "{error}");
        assert!(!scratch.join("real/missing.toml").exists());
    }

    #[test]
    fn the_replaced_file_is_a_new_file_so_other_names_keep_the_old_content() {
        let scratch = Scratch::new("atomic_unix_hard_link");
        let target = scratch.file("a.txt", "old");
        let other = scratch.join("b.txt");
        fs::hard_link(&target, &other).unwrap();
        let before = fs::metadata(&target).unwrap().ino();

        write_atomic(&target, b"new", &options(), &FakeClock::default()).unwrap();

        assert_ne!(fs::metadata(&target).unwrap().ino(), before);
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read(&other).unwrap(), b"old");
    }

    #[test]
    fn a_directory_that_cannot_be_written_fails_cleanly() {
        let scratch = Scratch::new("atomic_unix_no_write");
        let target = scratch.file("dir/file.txt", "old");
        let dir = scratch.join("dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let outcome = write_atomic(&target, b"new", &options(), &FakeClock::default());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        match outcome {
            Err(AtomicWriteError::Io { step, .. }) => {
                assert!(step.contains("temporary"), "{step}");
                assert_eq!(fs::read(&target).unwrap(), b"old");
            }
            // The superuser can write anywhere.
            Ok(()) => assert_eq!(fs::read(&target).unwrap(), b"new"),
            Err(error) => panic!("unexpected failure: {error}"),
        }
        assert_eq!(scratch.listing("dir"), ["file.txt"]);
    }
}

/// Files in use: `docs/testing-and-ci.md`, "Windows coverage", "Replacing a
/// file another process holds open retries and then fails cleanly."
#[cfg(windows)]
mod windows {
    use std::fs::{File, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    use cortex_base::{Clock, UtcTime};

    use super::*;

    /// Opens a file the way a program that wants it to itself does: nobody
    /// else may read, write or delete it while the handle is open.
    fn lock_exclusively(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .expect("open the file with no sharing")
    }

    #[test]
    fn replacing_a_file_that_stays_locked_retries_then_fails_cleanly() {
        let scratch = Scratch::new("atomic_windows_locked");
        let target = scratch.file("report.docx", "the original");
        let clock = FakeClock::default();

        let held = lock_exclusively(&target);
        let error = write_atomic(&target, b"the replacement", &options(), &clock).unwrap_err();

        match &error {
            AtomicWriteError::InUse {
                attempts, waited, ..
            } => {
                assert_eq!(*attempts, 7, "the first try and six retries");
                assert_eq!(*waited, Duration::from_millis(500));
            }
            other => panic!("expected the file to be in use, got: {other}"),
        }
        let message = error.to_string();
        assert!(message.contains("report.docx"), "{message}");
        assert!(message.contains("open in another program"), "{message}");
        assert_eq!(clock.sleeps().len(), 6, "it waited before each retry");
        assert_eq!(
            clock.slept(),
            Duration::from_millis(500),
            "on the fake clock"
        );

        // Nothing changed, and nothing was left behind.
        drop(held);
        assert_eq!(fs::read_to_string(&target).unwrap(), "the original");
        assert_eq!(scratch.listing(""), ["report.docx"]);

        // With the file let go, the same write goes through.
        write_atomic(&target, b"the replacement", &options(), &clock).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "the replacement");
        assert_eq!(clock.sleeps().len(), 6, "and waits for nothing");
    }

    /// A clock whose third sleep is the moment the other program closes the
    /// file.
    #[derive(Debug)]
    struct ReleasesOnThirdSleep {
        inner: FakeClock,
        held: Mutex<Option<File>>,
    }

    impl Clock for ReleasesOnThirdSleep {
        fn now(&self) -> UtcTime {
            self.inner.now()
        }
        fn monotonic(&self) -> Duration {
            self.inner.monotonic()
        }
        fn sleep(&self, duration: Duration) {
            self.inner.sleep(duration);
            if self.inner.sleeps().len() == 3 {
                self.held.lock().unwrap().take();
            }
        }
    }

    #[test]
    fn replacing_a_file_that_is_let_go_succeeds_after_the_retries_so_far() {
        let scratch = Scratch::new("atomic_windows_released");
        let target = scratch.file("report.docx", "the original");
        let clock = ReleasesOnThirdSleep {
            inner: FakeClock::default(),
            held: Mutex::new(Some(lock_exclusively(&target))),
        };

        write_atomic(&target, b"the replacement", &options(), &clock).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "the replacement");
        assert_eq!(
            clock.inner.sleeps(),
            [10, 20, 40].map(Duration::from_millis),
            "three waits, then the fourth attempt worked"
        );
        assert_eq!(scratch.listing(""), ["report.docx"]);
    }

    #[test]
    fn with_no_retries_a_locked_file_fails_at_once() {
        let scratch = Scratch::new("atomic_windows_no_retry");
        let target = scratch.file("data.bin", "old");
        let clock = FakeClock::default();
        let impatient = AtomicWriteOptions {
            retry_waits: Vec::new(),
            ..options()
        };
        let held = lock_exclusively(&target);
        let error = write_atomic(&target, b"new", &impatient, &clock).unwrap_err();
        assert!(
            matches!(error, AtomicWriteError::InUse { attempts: 1, .. }),
            "{error}"
        );
        assert!(clock.sleeps().is_empty());
        drop(held);
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(scratch.listing(""), ["data.bin"]);
    }

    #[test]
    fn a_file_another_program_is_only_reading_can_be_replaced() {
        // The standard library, like most modern programs, opens files in a
        // way that lets them be replaced. Only an exclusive hold blocks.
        let scratch = Scratch::new("atomic_windows_shared");
        let target = scratch.file("shared.txt", "old");
        let clock = FakeClock::default();
        let reader = File::open(&target).unwrap();
        write_atomic(&target, b"new", &options(), &clock).unwrap();
        drop(reader);
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert!(clock.sleeps().is_empty());
    }

    #[test]
    fn a_path_longer_than_260_characters_is_written() {
        let scratch = Scratch::new("atomic_windows_long");
        let resolver = scratch.resolver("ws");
        let deep = format!(
            "{}/file.txt",
            ["a-directory-with-a-long-name"; 12].join("/")
        );
        let resolved = resolver.resolve(&deep).unwrap();
        assert!(resolved.abs().as_str().len() > 300, "the path is long");
        assert!(!resolved.exists());

        let target = resolved.to_fs_path();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let clock = FakeClock::default();
        write_atomic(&target, b"deep", &options(), &clock).unwrap();
        write_atomic(&target, b"deeper", &options(), &clock).unwrap();

        let again = resolver.resolve(&deep).unwrap();
        assert!(again.exists());
        assert!(again.is_inside_workspace());
        assert_eq!(again, resolver.resolve(&deep.replace('/', "\\")).unwrap());
        assert_eq!(fs::read_to_string(again.to_fs_path()).unwrap(), "deeper");
        assert!(
            !again.display().starts_with(r"\\?\"),
            "the long form is not shown"
        );
    }
}
