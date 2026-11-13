//! The state directory on a real file system: created private, and
//! reported, not changed, when it is already there and is not.

use std::fs;

use cortex_base::{FakeClock, private};
use cortex_config::state::{StateDir, write_private};
use cortex_config::{Env, Platform, Severity};
use cortex_protocol::ProjectKey;

use crate::Scratch;

#[test]
fn a_missing_state_directory_is_created_for_the_user_alone() {
    let scratch = Scratch::new("state_created");
    let state = StateDir::new(scratch.join("local/state/cortex-harness"));
    assert!(
        state.privacy_warning().is_none(),
        "nothing there, nothing to warn about"
    );

    let report = state.ensure().unwrap();
    assert!(report.created && report.private);
    assert!(private::is_private(state.root()).unwrap());
    // The parents it had to create are the user's alone too.
    assert!(private::is_private(&scratch.join("local/state")).unwrap());
    assert!(state.privacy_warning().is_none());

    // Asking again finds it and changes nothing.
    let report = state.ensure().unwrap();
    assert!(!report.created && report.private);

    let project = ProjectKey::new("9f2c4a1b7d3e");
    let index = state.index_dir(&project).unwrap();
    state.ensure_dir(&index).unwrap();
    assert!(private::is_private(&index).unwrap());
    assert!(state.ensure_dir(&scratch.join("outside")).is_err());
    assert!(!scratch.join("outside").exists());
}

/// "Inside the state directory" is a matter of names, and `..` is not one:
/// the operating system follows it out. Before, a path that started with
/// the state directory's text and climbed out was created.
#[test]
fn ensure_dir_refuses_a_path_that_climbs_out() {
    let scratch = Scratch::new("state_dot_dot");
    let state = StateDir::new(scratch.join("state"));
    state.ensure().unwrap();
    for outside in [
        "state/../escaped",
        "state/projects/../../escaped",
        "state/projects/x/../../../escaped",
    ] {
        let error = state.ensure_dir(&scratch.join(outside)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{outside}");
        assert!(!scratch.join("escaped").exists(), "{outside}");
    }
    // A `..` that stays inside is refused too: there is no reason to write
    // one, and checking where it leads would mean asking the file system.
    assert!(state.ensure_dir(&scratch.join("state/a/../b")).is_err());
    assert!(!scratch.join("state/b").exists());
    // `.` is harmless.
    state.ensure_dir(&scratch.join("state/./logs")).unwrap();
    assert!(scratch.join("state/logs").is_dir());
}

#[test]
fn a_state_directory_that_is_open_to_others_is_reported_and_not_changed() {
    let scratch = Scratch::new("state_existing");
    let root = scratch.join("state");
    // Made the ordinary way: on Linux with the usual mode, on Windows with
    // the access list of the folder above.
    fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let state = StateDir::new(&root);

    let report = state.ensure().unwrap();
    assert!(!report.created);
    assert!(!report.private, "it is not private, and that is said");
    assert!(
        !private::is_private(&root).unwrap(),
        "and it was not changed"
    );

    let warning = state.privacy_warning().expect("a warning");
    assert_eq!(warning.severity, Severity::Warning);
    assert_eq!(warning.file.as_deref(), Some(root.as_path()));
    let text = warning.render(&Env::empty(Platform::CURRENT));
    assert!(text.contains("can be opened by other users"), "{text}");
    assert!(text.contains("It was not changed"), "{text}");

    // A private file in it is private by itself all the same.
    let trust = state.trust_file();
    write_private(&trust, b"# trust\n", &FakeClock::default()).unwrap();
    assert!(private::is_private(&trust).unwrap());
    assert_eq!(fs::read(&trust).unwrap(), b"# trust\n");

    // And a directory made inside it through the state directory is too.
    let logs = state.logs_dir();
    state.ensure_dir(&logs).unwrap();
    assert!(private::is_private(&logs).unwrap());
    assert!(!private::is_private(&root).unwrap());
}

#[test]
fn a_state_file_is_written_private_and_its_directory_is_made() {
    let scratch = Scratch::new("state_write");
    let state = StateDir::new(scratch.join("state"));
    let project = ProjectKey::new("abc123");
    let file = state.approvals_file(&project).unwrap();
    let clock = FakeClock::default();

    write_private(&file, b"one", &clock).unwrap();
    write_private(&file, b"two", &clock).unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"two");
    assert!(private::is_private(&file).unwrap());
    assert!(private::is_private(file.parent().unwrap()).unwrap());
    assert!(clock.sleeps().is_empty());
}
