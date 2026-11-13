//! The credential stores on a real file system, and on Windows in the real
//! Credential Manager
//! (`docs/architecture/08-configuration.md`, "Credentials").
//!
//! **No test touches a real credential.** The file store works in a folder
//! of the test's own. The Credential Manager test uses entries under a name
//! of its own that no real entry has, and removes them when it ends, pass
//! or fail. Every key here is a made-up string.

use std::fs;
use std::sync::Arc;

use cortex_base::{Classified, ErrorKind, FakeClock, Secret, SharedClock, private};
use cortex_config::credentials::{self, CredentialSource, Lookup};
use cortex_config::{CredentialStore, Env, FileStore, Platform, StoreKind};

use crate::{Scratch, make_link, write};

const KEY_ONE: &str = "sk-test-0001-alpha";
const KEY_TWO: &str = "sk-test-0002-bravo";

fn clock() -> SharedClock {
    Arc::new(FakeClock::default())
}

fn store(scratch: &Scratch) -> FileStore {
    // The configuration directory does not exist yet: the first write
    // creates it.
    FileStore::new(
        scratch.join("config/cortex-harness/credentials.toml"),
        clock(),
    )
}

fn names(scratch: &Scratch, directory: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(scratch.join(directory))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_key_round_trips_through_the_file_store() {
    let scratch = Scratch::new("cred_round_trip");
    let store = store(&scratch);
    assert_eq!(store.kind(), StoreKind::File);
    assert!(store.get("example").unwrap().is_none(), "no file yet");
    assert!(!store.remove("example").unwrap());
    assert!(!scratch.join("config").exists(), "asking creates nothing");
    assert_eq!(store.is_private().unwrap(), None);

    store.set("example", &Secret::new(KEY_ONE)).unwrap();
    store
        .set("other.provider-2", &Secret::new(KEY_TWO))
        .unwrap();
    assert_eq!(store.get("example").unwrap().unwrap().expose(), KEY_ONE);
    assert_eq!(
        store.get("other.provider-2").unwrap().unwrap().expose(),
        KEY_TWO
    );
    assert!(store.get("absent").unwrap().is_none());

    // Replaced, then removed, and the other one is untouched throughout.
    store
        .set("example", &Secret::new("sk-test-0003-charlie"))
        .unwrap();
    assert_eq!(
        store.get("example").unwrap().unwrap().expose(),
        "sk-test-0003-charlie"
    );
    assert!(store.remove("example").unwrap());
    assert!(!store.remove("example").unwrap(), "already gone");
    assert!(store.get("example").unwrap().is_none());
    assert_eq!(
        store.get("other.provider-2").unwrap().unwrap().expose(),
        KEY_TWO
    );

    // A second store on the same file, as a second process would have.
    let again = FileStore::new(store.path(), clock());
    assert_eq!(
        again.get("other.provider-2").unwrap().unwrap().expose(),
        KEY_TWO
    );
    assert_eq!(
        names(&scratch, "config/cortex-harness"),
        ["credentials.toml", "credentials.toml.lock"],
        "no temporary file is left"
    );
}

#[test]
fn the_file_store_is_owner_only() {
    let scratch = Scratch::new("cred_private");
    let store = store(&scratch);
    store.set("example", &Secret::new(KEY_ONE)).unwrap();

    assert_eq!(store.is_private().unwrap(), Some(true));
    assert!(private::is_private(store.path()).unwrap());
    let lock = scratch.join("config/cortex-harness/credentials.toml.lock");
    assert!(private::is_private(&lock).unwrap(), "the lock file too");
    // The directory was missing, so it was made for the user alone.
    assert!(private::is_private(&scratch.join("config/cortex-harness")).unwrap());

    // Still private after every kind of change.
    store.set("second", &Secret::new(KEY_TWO)).unwrap();
    store.remove("example").unwrap();
    assert_eq!(store.is_private().unwrap(), Some(true));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(store.path()), 0o600);
        assert_eq!(mode(&lock), 0o600);
        assert_eq!(mode(&scratch.join("config/cortex-harness")), 0o700);
    }
    #[cfg(windows)]
    {
        let list = private::access_list(store.path()).unwrap();
        assert!(!list.inherits, "{list:?}");
        assert!(list.owned_by_current_user, "{list:?}");
        assert_eq!(list.entries.len(), 1, "{list:?}");
        assert!(list.entries[0].current_user, "{list:?}");
        assert_eq!(list.entries[0].kind, private::AccessKind::Allow, "{list:?}");
    }
}

/// A credentials file someone made by hand, readable by others, in a
/// directory that is not private either. The key in it is still read. The
/// first write makes the file private and leaves the directory alone.
#[test]
fn a_file_made_by_hand_is_read_reported_and_made_private_by_the_next_write() {
    let scratch = Scratch::new("cred_by_hand");
    let store = store(&scratch);
    write(
        store.path(),
        &format!("[providers.example]\nkey = \"{KEY_ONE}\"\n"),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
    }
    let directory = scratch.join("config/cortex-harness");
    let directory_before = private::is_private(&directory).unwrap();

    assert_eq!(store.is_private().unwrap(), Some(false), "reported");
    assert_eq!(store.get("example").unwrap().unwrap().expose(), KEY_ONE);
    // `cortex auth status` gets the same fact in words, with the key's
    // source, and never the key.
    let env = Env::empty(Platform::CURRENT);
    let lookup = Lookup {
        provider: "example",
        flag: None,
        variable: None,
        env: &env,
        store: &store,
    };
    let status = credentials::status(lookup).unwrap();
    let warning = status.store_warning.clone().expect("a warning");
    assert!(
        warning.contains("can be opened by other users"),
        "{warning}"
    );
    assert!(warning.contains("credentials.toml"), "{warning}");
    assert!(!format!("{status:?}").contains(KEY_ONE));

    store.set("second", &Secret::new(KEY_TWO)).unwrap();
    assert_eq!(
        store.is_private().unwrap(),
        Some(true),
        "closed by the write"
    );
    assert_eq!(store.get("example").unwrap().unwrap().expose(), KEY_ONE);
    assert_eq!(credentials::status(lookup).unwrap().store_warning, None);
    assert_eq!(
        private::is_private(&directory).unwrap(),
        directory_before,
        "a directory that was there keeps what it had"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o7777,
            0o600
        );
    }
}

#[test]
fn concurrent_writers_do_not_corrupt_the_file_or_lose_each_others_keys() {
    const WRITERS: usize = 4;
    const ROUNDS: usize = 3;
    let scratch = Scratch::new("cred_concurrent");
    let path = store(&scratch).path().to_path_buf();

    // Each writer is a store of its own, as a separate process would be:
    // nothing is shared but the files.
    let writers: Vec<_> = (0..WRITERS)
        .map(|writer| {
            let path = path.clone();
            std::thread::spawn(move || {
                let store = FileStore::new(path, clock());
                for round in 0..ROUNDS {
                    // Its own provider, and one that every writer fights over.
                    let key = Secret::new(format!("sk-test-w{writer}-r{round}"));
                    store.set(&format!("writer-{writer}"), &key).unwrap();
                    store.set("shared", &key).unwrap();
                    // Reads go on throughout and never see half a file.
                    assert!(store.get("shared").unwrap().is_some());
                    if round == 1 {
                        assert!(store.remove(&format!("writer-{writer}")).unwrap());
                    }
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().expect("a writer panicked");
    }

    // The file is whole, every writer's last key is there, and the shared
    // one is one of the keys that was written, not a mixture.
    let store = FileStore::new(&path, clock());
    let text = fs::read_to_string(&path).unwrap();
    let parsed: toml::Table = text.parse().expect("the file is valid TOML");
    assert_eq!(parsed["version"].as_integer(), Some(1));
    assert_eq!(parsed["providers"].as_table().unwrap().len(), WRITERS + 1);
    for writer in 0..WRITERS {
        assert_eq!(
            store
                .get(&format!("writer-{writer}"))
                .unwrap()
                .unwrap()
                .expose(),
            format!("sk-test-w{writer}-r{}", ROUNDS - 1),
            "writer {writer}'s last key was lost"
        );
    }
    let shared = store.get("shared").unwrap().unwrap();
    assert!(
        (0..WRITERS).any(|writer| shared.expose() == format!("sk-test-w{writer}-r{}", ROUNDS - 1)),
        "the shared key is some writer's last"
    );
    assert_eq!(store.is_private().unwrap(), Some(true));
    assert_eq!(
        names(&scratch, "config/cortex-harness"),
        ["credentials.toml", "credentials.toml.lock"],
        "no temporary file is left"
    );
}

#[test]
fn what_is_not_understood_is_refused_and_left_alone() {
    let scratch = Scratch::new("cred_malformed");
    let store = store(&scratch);
    for (content, expected) in [
        (
            format!("version = 2\n[providers.example]\nkey = \"{KEY_ONE}\"\n"),
            "written by a newer version",
        ),
        (
            format!("[providers.example]\nkey = \"{KEY_ONE}\nbroken = \n"),
            "is not valid TOML",
        ),
        (
            format!("providers = \"{KEY_ONE}\"\n"),
            "`providers` is not a table",
        ),
        (
            format!("version = \"{KEY_ONE}\"\n"),
            "its `version` is not one",
        ),
    ] {
        write(store.path(), &content);
        for error in [
            store.get("example").unwrap_err(),
            store.set("example", &Secret::new(KEY_TWO)).unwrap_err(),
            store.remove("example").unwrap_err(),
        ] {
            let text = format!("{error} / {error:?}");
            assert!(text.contains(expected), "{expected:?} not in {text}");
            assert_eq!(error.kind(), ErrorKind::Config);
            // No error repeats what is in the file, in any form.
            assert!(!text.contains(KEY_ONE), "the key leaked into: {text}");
            assert!(
                !text.contains("sk-test"),
                "part of a key leaked into: {text}"
            );
        }
        assert_eq!(
            fs::read_to_string(store.path()).unwrap(),
            content,
            "the file is exactly as it was"
        );
    }

    // A key that is not text is an error for that provider only.
    write(
        store.path(),
        &format!("[providers.bad]\nkey = 12345\n[providers.good]\nkey = \"{KEY_ONE}\"\n"),
    );
    assert!(
        store
            .get("bad")
            .unwrap_err()
            .to_string()
            .contains("the key of `bad` is not text")
    );
    assert_eq!(store.get("good").unwrap().unwrap().expose(), KEY_ONE);
}

#[test]
fn what_this_version_does_not_know_is_kept_when_the_file_is_rewritten() {
    let scratch = Scratch::new("cred_unknown_kept");
    let store = store(&scratch);
    write(
        store.path(),
        &format!(
            "# my own note\nversion = 1\nfuture = \"setting\"\n\n\
             [providers.example]\nkey = \"{KEY_ONE}\"\nexpires = \"2027-01-01\"\n\n\
             [tokens.somewhere]\nrefresh = \"sk-test-refresh\"\n"
        ),
    );
    store.set("second", &Secret::new(KEY_TWO)).unwrap();

    let parsed: toml::Table = fs::read_to_string(store.path()).unwrap().parse().unwrap();
    assert_eq!(parsed["future"].as_str(), Some("setting"));
    assert_eq!(
        parsed["providers"]["example"]["expires"].as_str(),
        Some("2027-01-01")
    );
    assert_eq!(
        parsed["tokens"]["somewhere"]["refresh"].as_str(),
        Some("sk-test-refresh")
    );
    assert_eq!(parsed["providers"]["second"]["key"].as_str(), Some(KEY_TWO));
    assert!(
        fs::read_to_string(store.path())
            .unwrap()
            .starts_with("# Cortex-Harness credentials.")
    );
}

#[test]
fn a_store_and_its_errors_never_print_a_key() {
    let scratch = Scratch::new("cred_no_print");
    let store = store(&scratch);
    store.set("example", &Secret::new(KEY_ONE)).unwrap();
    let env = Env::empty(Platform::CURRENT);
    let found = store.get("example").unwrap();
    let invalid = store.set("no/slash", &Secret::new(KEY_ONE)).unwrap_err();
    assert_eq!(invalid.kind(), ErrorKind::Usage);
    let printed = format!(
        "{store:?} {} {found:?} {invalid} {invalid:?}",
        store.describe(&env)
    );
    assert!(!printed.contains(KEY_ONE), "{printed}");
    assert!(!printed.contains("sk-test"), "{printed}");
    assert!(printed.contains("[redacted]"), "{printed}");
    assert!(store.describe(&env).starts_with("the file "));

    // And the lookup through the store reports the source, not the key.
    let credential = credentials::resolve(Lookup {
        provider: "example",
        flag: None,
        variable: Some("EXAMPLE_API_KEY"),
        env: &env,
        store: &store,
    })
    .unwrap()
    .unwrap();
    assert_eq!(credential.source, CredentialSource::Store(StoreKind::File));
    assert_eq!(credential.secret.expose(), KEY_ONE);
    assert!(!format!("{credential:?}").contains(KEY_ONE));
}

/// A link at the credentials file's path, or at its lock's, is refused
/// with an error that says so, and nothing is written where it points.
/// Before, the key was written into whatever file the link named: one in a
/// repository of dotfiles, say.
#[test]
fn a_link_at_the_credentials_path_or_its_lock_is_refused() {
    let scratch = Scratch::new("cred_link");
    let elsewhere = scratch.join("dotfiles/notes.toml");
    write(&elsewhere, "# notes\n");
    let store = store(&scratch);
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    if !make_link(&elsewhere, store.path(), false) {
        return;
    }
    let refused = |result: Result<(), cortex_config::CredentialError>, what: &str| {
        let error = result.expect_err(what);
        assert!(
            matches!(&error, cortex_config::CredentialError::Link { path } if path == store.path()),
            "{what}: {error:?}"
        );
        assert_eq!(error.kind(), ErrorKind::Config);
        assert!(error.to_string().contains("is a link"), "{error}");
    };
    refused(store.set("example", &Secret::new(KEY_ONE)), "set");
    refused(store.get("example").map(drop), "get");
    refused(store.remove("example").map(drop), "remove");
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "# notes\n");
    assert!(
        fs::symlink_metadata(store.path())
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link is left as it is"
    );
    let env = Env::empty(Platform::CURRENT);
    assert!(
        store.warning(&env).unwrap().contains("is a link"),
        "auth status says why"
    );

    // Refused before anything was made: there is no lock file.
    let mut lock = store.path().as_os_str().to_owned();
    lock.push(".lock");
    let lock = std::path::PathBuf::from(lock);
    assert!(fs::symlink_metadata(&lock).is_err());

    // The lock file's name, linked to the same file.
    fs::remove_file(store.path()).unwrap();
    assert!(make_link(&elsewhere, &lock, false));
    let error = store.set("example", &Secret::new(KEY_ONE)).unwrap_err();
    assert!(
        matches!(&error, cortex_config::CredentialError::Link { path } if *path == lock),
        "{error:?}"
    );
    assert!(!store.path().exists(), "nothing was written");
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "# notes\n");
}

/// The real Windows Credential Manager, with entries under a name of the
/// test's own.
#[cfg(windows)]
mod windows {
    use cortex_base::{Rng, SystemRng};
    use cortex_config::credentials::CredentialManagerStore;

    use super::*;

    /// Removes the test's entries when the test ends, pass or fail.
    struct Cleanup<'a> {
        store: &'a CredentialManagerStore,
        providers: &'a [&'a str],
    }

    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            for provider in self.providers {
                let _ = self.store.remove(provider);
            }
        }
    }

    #[test]
    fn a_key_round_trips_through_credential_manager() {
        // A prefix no real entry has: the harness's own entries start with
        // `cortex-harness:`, and no two runs share this one.
        let prefix = format!("cortex-harness-test-{:016x}:", SystemRng::new().next_u64());
        let store = CredentialManagerStore::with_prefix(&prefix);
        let providers = ["example", "other.provider-2"];
        let _cleanup = Cleanup {
            store: &store,
            providers: &providers,
        };
        assert_eq!(store.kind(), StoreKind::CredentialManager);
        assert_eq!(store.target("example"), format!("{prefix}example"));

        assert!(store.get("example").unwrap().is_none());
        assert!(!store.remove("example").unwrap());

        store.set("example", &Secret::new(KEY_ONE)).unwrap();
        store
            .set("other.provider-2", &Secret::new(KEY_TWO))
            .unwrap();
        assert_eq!(store.get("example").unwrap().unwrap().expose(), KEY_ONE);
        assert_eq!(
            store.get("other.provider-2").unwrap().unwrap().expose(),
            KEY_TWO
        );

        // Replaced in place, read through a second store as a second
        // process would, then removed.
        store
            .set("example", &Secret::new("sk-test-0003-charlie"))
            .unwrap();
        let again = CredentialManagerStore::with_prefix(&prefix);
        assert_eq!(
            again.get("example").unwrap().unwrap().expose(),
            "sk-test-0003-charlie"
        );
        assert!(store.remove("example").unwrap());
        assert!(store.get("example").unwrap().is_none());
        assert!(!store.remove("example").unwrap());
        assert_eq!(
            store.get("other.provider-2").unwrap().unwrap().expose(),
            KEY_TWO
        );

        // The harness's own entries are under another name, so this store
        // cannot have seen or changed one.
        assert!(!prefix.starts_with("cortex-harness:"));
        assert_ne!(
            CredentialManagerStore::new().target("example"),
            store.target("example")
        );

        let printed = format!(
            "{store:?} {}",
            store.describe(&Env::empty(Platform::Windows))
        );
        assert!(!printed.contains("sk-test"), "{printed}");
        let invalid = store.set("no/slash", &Secret::new(KEY_ONE)).unwrap_err();
        assert_eq!(invalid.kind(), ErrorKind::Usage);
    }
}
