//! Credentials: where API keys are kept and how one is found
//! (`docs/architecture/08-configuration.md`, "Credentials", and
//! `docs/decisions/0018-credential-storage.md`).
//!
//! **Lookup order:** a command-line flag, then the provider's environment
//! variable, then the credential store. [`resolve`] applies it and says
//! which source gave the key, never the key.
//!
//! **The stores.**
//!
//! | OS | Default | Alternative |
//! |---|---|---|
//! | Windows | Windows Credential Manager | [`FileStore`] |
//! | Linux | [`FileStore`]: an owner-only `credentials.toml` | The Secret Service keychain, in a build that has it. No build does yet. |
//!
//! **In memory** a credential is a [`Secret`]: it cannot be printed and
//! cannot be serialised. No error in this module carries one, and a store's
//! own error text is passed on only where it cannot hold one.
//!
//! **Where this ends.** The `AuthMethod` trait, which puts a credential on
//! a request, is in `cortex-llm`, and this crate may not depend on that
//! one. This module finds the key and describes its source; the core hands
//! both to the provider layer's API-key `AuthMethod`. A sign-in flow added
//! later stores its tokens through the same [`CredentialStore`].

mod file;
#[cfg(windows)]
mod windows;

use std::fmt;
use std::path::PathBuf;

use cortex_base::{Classified, ErrorKind, Secret, SharedClock};

pub use file::FileStore;
#[cfg(windows)]
pub use windows::CredentialManagerStore;

use crate::dirs::Dirs;
use crate::env::Env;
use crate::schema::StoreChoice;

/// Which kind of store a credential is kept in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// An owner-only file, `credentials.toml`.
    File,
    /// Windows Credential Manager.
    CredentialManager,
}

impl StoreKind {
    /// The store's name, for `cortex auth status`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "credentials file",
            Self::CredentialManager => "Windows Credential Manager",
        }
    }
}

impl fmt::Display for StoreKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a credential could not be read, stored or removed. No variant holds
/// a credential, and no message contains one.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The provider's name cannot be used to store a credential under.
    #[error(
        "`{name}` cannot name a provider here: use letters, digits, `-`, `_` and `.`, starting \
         with a letter or a digit, at most 64 characters"
    )]
    InvalidProvider {
        /// The name.
        name: String,
    },
    /// The store this configuration asks for is not in this build or on
    /// this machine.
    #[error("{reason}")]
    Unavailable {
        /// Why, and what to do about it.
        reason: String,
    },
    /// A file of the file store could not be read or written.
    #[error("could not {action} `{path}`: {source}")]
    Io {
        /// What was being done, as a phrase: "read the credentials file".
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// What the operating system reported.
        #[source]
        source: std::io::Error,
    },
    /// The credentials file is not what this version writes. It is left as
    /// it is: nothing is overwritten that was not understood.
    #[error("the credentials file `{path}` cannot be used: {reason}")]
    Malformed {
        /// The file.
        path: PathBuf,
        /// What is wrong. Never the file's content.
        reason: String,
    },
    /// The operating system's store refused or failed.
    #[error("{store} failed to {action}: {reason}")]
    Store {
        /// The store.
        store: StoreKind,
        /// What was being done: "read the key".
        action: &'static str,
        /// What it reported. Never a stored value.
        reason: String,
    },
}

impl Classified for CredentialError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidProvider { .. } => ErrorKind::Usage,
            Self::Unavailable { .. } | Self::Malformed { .. } => ErrorKind::Config,
            Self::Io { .. } | Self::Store { .. } => ErrorKind::Auth,
        }
    }
}

/// A place credentials are kept between runs, one per provider.
///
/// Every method blocks, and is called on the blocking pool.
pub trait CredentialStore: fmt::Debug + Send + Sync {
    /// Which kind of store this is.
    fn kind(&self) -> StoreKind;

    /// The store in words, for `cortex auth status`: which file, or which
    /// service. Paths under the home directory are written with `~`.
    fn describe(&self, env: &Env) -> String;

    /// Something the user should know about the store itself, as a
    /// sentence, or `None` when all is well. The file store says here that
    /// its file can be opened by other users. `cortex auth status` and the
    /// diagnostics show it.
    fn warning(&self, env: &Env) -> Option<String> {
        let _ = env;
        None
    }

    /// The stored credential of a provider, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// When the store cannot be read, or holds something this version does
    /// not understand.
    fn get(&self, provider: &str) -> Result<Option<Secret>, CredentialError>;

    /// Stores a provider's credential, replacing the one that was there.
    ///
    /// # Errors
    ///
    /// When the store cannot be written. What was stored before is then
    /// still there.
    fn set(&self, provider: &str, credential: &Secret) -> Result<(), CredentialError>;

    /// Removes a provider's credential. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// When the store cannot be written.
    fn remove(&self, provider: &str) -> Result<bool, CredentialError>;
}

/// Checks that a provider's name can be a key in the credentials file and
/// part of a name in the operating system's store.
///
/// # Errors
///
/// [`CredentialError::InvalidProvider`] otherwise.
pub fn check_provider(name: &str) -> Result<(), CredentialError> {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric());
    let rest = bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if first && rest && name.len() <= 64 {
        Ok(())
    } else {
        Err(CredentialError::InvalidProvider {
            name: name.to_owned(),
        })
    }
}

/// Opens the store the configuration asks for.
///
/// `auto` is Windows Credential Manager on Windows and the owner-only file
/// on Linux. Opening touches nothing: a file store's file is created by its
/// first `set`.
///
/// # Errors
///
/// [`CredentialError::Unavailable`] when the store is not in this build or
/// cannot be placed: the keychain on Linux, or a file store with no
/// directory to live in.
pub fn open_store(
    choice: StoreChoice,
    dirs: &Dirs,
    clock: SharedClock,
) -> Result<Box<dyn CredentialStore>, CredentialError> {
    let file = |clock: SharedClock| -> Result<Box<dyn CredentialStore>, CredentialError> {
        match dirs.credentials_file() {
            Some(path) => Ok(Box::new(FileStore::new(path, clock))),
            None => Err(CredentialError::Unavailable {
                reason: "there is no directory to keep the credentials file in: \
                         the home directory is not known. Set the provider's environment variable \
                         instead"
                    .to_owned(),
            }),
        }
    };
    match choice {
        StoreChoice::File => file(clock),
        StoreChoice::Auto if cfg!(windows) => keychain(),
        StoreChoice::Auto => file(clock),
        StoreChoice::Keychain => keychain(),
    }
}

#[cfg(windows)]
fn keychain() -> Result<Box<dyn CredentialStore>, CredentialError> {
    Ok(Box::new(CredentialManagerStore::new()))
}

#[cfg(not(windows))]
fn keychain() -> Result<Box<dyn CredentialStore>, CredentialError> {
    Err(CredentialError::Unavailable {
        reason: "`credentials.store = \"keychain\"` cannot be used on this system yet: the \
                 Secret Service keychain is not built into any version of Cortex-Harness so far, \
                 this one included. Use `credentials.store = \"file\"`, which is the default \
                 here and keeps keys in a file only you can read, or set the provider's \
                 environment variable"
            .to_owned(),
    })
}

/// Where a credential in use came from. Never the credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// A command-line flag.
    Flag,
    /// An environment variable.
    Environment {
        /// The variable's name.
        variable: String,
    },
    /// The credential store.
    Store(StoreKind),
}

impl fmt::Display for CredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flag => f.write_str("a command-line flag"),
            Self::Environment { variable } => write!(f, "the environment variable {variable}"),
            Self::Store(store) => write!(f, "the {store}"),
        }
    }
}

/// A credential and where it came from.
#[derive(Debug, Clone)]
pub struct Credential {
    /// The credential. It prints as `[redacted]`.
    pub secret: Secret,
    /// Where it came from.
    pub source: CredentialSource,
}

/// What `cortex auth status` shows for one provider. It holds no
/// credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStatus {
    /// The provider.
    pub provider: String,
    /// The source that is used, or `None` when there is no credential.
    pub active: Option<CredentialSource>,
    /// Whether the store holds a credential for the provider.
    pub stored: bool,
    /// Whether a stored credential is there and is not the one in use,
    /// because the environment variable or a flag comes first.
    pub stored_but_unused: bool,
    /// What the store has to say about itself: a credentials file that
    /// other users can open.
    pub store_warning: Option<String>,
}

/// Everything a lookup needs.
#[derive(Debug, Clone, Copy)]
pub struct Lookup<'a> {
    /// The provider.
    pub provider: &'a str,
    /// The key given by a command-line flag, if one was.
    pub flag: Option<&'a Secret>,
    /// The provider's environment variable: its built-in one, or the
    /// `key_env` of its configuration entry.
    pub variable: Option<&'a str>,
    /// The environment.
    pub env: &'a Env,
    /// The store.
    pub store: &'a dyn CredentialStore,
}

/// Finds a provider's credential: the flag, then the environment variable,
/// then the store. A flag or a variable that is empty counts as not given.
/// `None` when no source has one.
///
/// The store is not asked when an earlier source answers, so a broken or
/// locked store does not get in the way of a key from the environment.
///
/// # Errors
///
/// When the store has to be asked and fails.
pub fn resolve(lookup: Lookup<'_>) -> Result<Option<Credential>, CredentialError> {
    check_provider(lookup.provider)?;
    if let Some(found) = before_the_store(lookup) {
        return Ok(Some(found));
    }
    Ok(lookup.store.get(lookup.provider)?.map(|secret| Credential {
        secret,
        source: CredentialSource::Store(lookup.store.kind()),
    }))
}

/// Says which source a provider's credential would come from, and whether
/// the store holds one that is passed over.
///
/// # Errors
///
/// When the store fails.
pub fn status(lookup: Lookup<'_>) -> Result<CredentialStatus, CredentialError> {
    check_provider(lookup.provider)?;
    let earlier = before_the_store(lookup).map(|found| found.source);
    let stored = lookup.store.get(lookup.provider)?.is_some();
    Ok(CredentialStatus {
        provider: lookup.provider.to_owned(),
        stored_but_unused: stored && earlier.is_some(),
        active: earlier.or_else(|| stored.then(|| CredentialSource::Store(lookup.store.kind()))),
        stored,
        store_warning: lookup.store.warning(lookup.env),
    })
}

fn before_the_store(lookup: Lookup<'_>) -> Option<Credential> {
    if let Some(flag) = lookup.flag.filter(|flag| !flag.is_empty()) {
        return Some(Credential {
            secret: flag.clone(),
            source: CredentialSource::Flag,
        });
    }
    let variable = lookup.variable?;
    lookup.env.get(variable).map(|value| Credential {
        secret: Secret::new(value),
        source: CredentialSource::Environment {
            variable: variable.to_owned(),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::env::Platform;

    /// A store in memory, for the lookup order. It counts how often it is
    /// asked, and can be made to fail.
    #[derive(Debug, Default)]
    struct MemoryStore {
        keys: Mutex<BTreeMap<String, String>>,
        asked: Mutex<u32>,
        broken: bool,
    }

    impl CredentialStore for MemoryStore {
        fn kind(&self) -> StoreKind {
            StoreKind::File
        }

        fn describe(&self, _: &Env) -> String {
            "a store in memory".to_owned()
        }

        fn get(&self, provider: &str) -> Result<Option<Secret>, CredentialError> {
            *self.asked.lock().unwrap() += 1;
            if self.broken {
                return Err(CredentialError::Store {
                    store: StoreKind::File,
                    action: "read the key",
                    reason: "it is broken".to_owned(),
                });
            }
            Ok(self.keys.lock().unwrap().get(provider).map(Secret::new))
        }

        fn set(&self, provider: &str, credential: &Secret) -> Result<(), CredentialError> {
            self.keys
                .lock()
                .unwrap()
                .insert(provider.to_owned(), credential.expose().to_owned());
            Ok(())
        }

        fn remove(&self, provider: &str) -> Result<bool, CredentialError> {
            Ok(self.keys.lock().unwrap().remove(provider).is_some())
        }
    }

    const STORED: &str = "sk-test-stored-0001";
    const FROM_ENV: &str = "sk-test-env-0002";
    const FROM_FLAG: &str = "sk-test-flag-0003";

    fn lookup<'a>(env: &'a Env, store: &'a MemoryStore, flag: Option<&'a Secret>) -> Lookup<'a> {
        Lookup {
            provider: "example",
            flag,
            variable: Some("EXAMPLE_API_KEY"),
            env,
            store,
        }
    }

    #[test]
    fn the_flag_comes_first_then_the_variable_then_the_store() {
        let store = MemoryStore::default();
        store.set("example", &Secret::new(STORED)).unwrap();
        let bare = Env::empty(Platform::Unix);
        let with_variable = bare.clone().with("EXAMPLE_API_KEY", FROM_ENV);
        let flag = Secret::new(FROM_FLAG);

        let found = resolve(lookup(&with_variable, &store, Some(&flag)))
            .unwrap()
            .unwrap();
        assert_eq!(found.secret.expose(), FROM_FLAG);
        assert_eq!(found.source, CredentialSource::Flag);

        let found = resolve(lookup(&with_variable, &store, None))
            .unwrap()
            .unwrap();
        assert_eq!(found.secret.expose(), FROM_ENV);
        assert_eq!(
            found.source,
            CredentialSource::Environment {
                variable: "EXAMPLE_API_KEY".to_owned()
            }
        );
        assert_eq!(*store.asked.lock().unwrap(), 0, "the store was not needed");

        let found = resolve(lookup(&bare, &store, None)).unwrap().unwrap();
        assert_eq!(found.secret.expose(), STORED);
        assert_eq!(found.source, CredentialSource::Store(StoreKind::File));

        store.remove("example").unwrap();
        assert!(resolve(lookup(&bare, &store, None)).unwrap().is_none());
    }

    #[test]
    fn an_empty_flag_or_variable_counts_as_not_given() {
        let store = MemoryStore::default();
        store.set("example", &Secret::new(STORED)).unwrap();
        let env = Env::empty(Platform::Unix).with("EXAMPLE_API_KEY", "");
        let empty = Secret::new("");
        let found = resolve(lookup(&env, &store, Some(&empty)))
            .unwrap()
            .unwrap();
        assert_eq!(found.source, CredentialSource::Store(StoreKind::File));

        // A provider with no variable goes straight to the store.
        let found = resolve(Lookup {
            variable: None,
            ..lookup(&env, &store, None)
        })
        .unwrap()
        .unwrap();
        assert_eq!(found.secret.expose(), STORED);
    }

    #[test]
    fn a_broken_store_does_not_get_in_the_way_of_the_environment() {
        let store = MemoryStore {
            broken: true,
            ..MemoryStore::default()
        };
        let env = Env::empty(Platform::Unix).with("EXAMPLE_API_KEY", FROM_ENV);
        assert!(resolve(lookup(&env, &store, None)).unwrap().is_some());
        let error = resolve(lookup(&Env::empty(Platform::Unix), &store, None)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Auth);
        assert_eq!(
            error.to_string(),
            "credentials file failed to read the key: it is broken"
        );
    }

    #[test]
    fn status_says_which_source_is_used_and_what_is_passed_over() {
        let store = MemoryStore::default();
        let bare = Env::empty(Platform::Unix);
        let with_variable = bare.clone().with("EXAMPLE_API_KEY", FROM_ENV);

        let nothing = status(lookup(&bare, &store, None)).unwrap();
        assert_eq!(
            (&nothing.active, nothing.stored, nothing.stored_but_unused),
            (&None, false, false)
        );

        store.set("example", &Secret::new(STORED)).unwrap();
        let stored = status(lookup(&bare, &store, None)).unwrap();
        assert_eq!(
            stored.active,
            Some(CredentialSource::Store(StoreKind::File))
        );
        assert!(stored.stored && !stored.stored_but_unused);

        // Both exist: the variable is used, and the status says a stored
        // key is being passed over.
        let both = status(lookup(&with_variable, &store, None)).unwrap();
        assert_eq!(
            both.active,
            Some(CredentialSource::Environment {
                variable: "EXAMPLE_API_KEY".to_owned()
            })
        );
        assert!(both.stored && both.stored_but_unused);

        // Nothing a status holds or prints is a key.
        let printed = format!(
            "{nothing:?}{stored:?}{both:?}{}",
            both.active.as_ref().unwrap()
        );
        for key in [STORED, FROM_ENV, FROM_FLAG] {
            assert!(!printed.contains(key), "{printed}");
        }
    }

    #[test]
    fn a_credential_does_not_print_its_secret() {
        let credential = Credential {
            secret: Secret::new(STORED),
            source: CredentialSource::Flag,
        };
        let printed = format!("{credential:?} {credential:#?}");
        assert!(!printed.contains(STORED), "{printed}");
        assert!(printed.contains("[redacted]"), "{printed}");
        assert_eq!(CredentialSource::Flag.to_string(), "a command-line flag");
        assert_eq!(
            CredentialSource::Store(StoreKind::CredentialManager).to_string(),
            "the Windows Credential Manager"
        );
    }

    #[test]
    fn a_provider_name_is_plain() {
        for good in [
            "anthropic",
            "openai",
            "my-server",
            "my.server",
            "local_2",
            "A1",
            "9lives",
        ] {
            assert!(check_provider(good).is_ok(), "{good}");
        }
        let long = "p".repeat(65);
        for bad in [
            "",
            "-leading",
            ".hidden",
            "_x",
            "with space",
            "a/b",
            "a\\b",
            "a:b",
            "a\nb",
            "ключ",
            "a\"b",
            "a]b",
            long.as_str(),
        ] {
            let error = check_provider(bad).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{bad}");
        }
        let store = MemoryStore::default();
        let env = Env::empty(Platform::Unix);
        let bad = Lookup {
            provider: "../etc",
            ..lookup(&env, &store, None)
        };
        assert!(resolve(bad).is_err());
        assert!(status(bad).is_err());
        assert_eq!(*store.asked.lock().unwrap(), 0);
    }

    #[test]
    fn the_store_is_chosen_by_configuration_and_by_os() {
        use cortex_base::SystemClock;
        use std::sync::Arc;

        let clock = || -> SharedClock { Arc::new(SystemClock::new()) };
        let dirs = Dirs::from_env(&Env::empty(Platform::CURRENT).with(
            if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "XDG_CONFIG_HOME"
            },
            if cfg!(windows) { r"C:\local" } else { "/cfg" },
        ));
        let nowhere = Dirs::from_env(&Env::empty(Platform::CURRENT));

        let file = open_store(StoreChoice::File, &dirs, clock()).unwrap();
        assert_eq!(file.kind(), StoreKind::File);
        let error = open_store(StoreChoice::File, &nowhere, clock()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Config);
        assert!(
            error.to_string().contains("no directory to keep the credentials file in"),
            "{error}"
        );

        let auto = open_store(StoreChoice::Auto, &dirs, clock()).unwrap();
        let keychain = open_store(StoreChoice::Keychain, &dirs, clock());
        if cfg!(windows) {
            assert_eq!(auto.kind(), StoreKind::CredentialManager);
            assert_eq!(keychain.unwrap().kind(), StoreKind::CredentialManager);
        } else {
            assert_eq!(auto.kind(), StoreKind::File);
            let error = keychain.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Config);
            let text = error.to_string();
            assert!(
                text.contains("not built into any version")
                    && text.contains("credentials.store = \"file\""),
                "the message says it is not available yet and what to use: {text}"
            );
        }
    }
}
