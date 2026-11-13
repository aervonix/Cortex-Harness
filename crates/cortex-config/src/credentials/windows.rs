//! Windows Credential Manager, the default store on Windows.
//!
//! Each provider's key is one *generic credential*, named
//! `cortex-harness:<provider>`. It appears under that name in the Windows
//! "Credential Manager" control panel, where the user can see that it is
//! there and delete it. Windows keeps it encrypted for the user's account.
//!
//! - **This machine only.** A credential is stored with local-machine
//!   persistence. The wrapper's own default would let Windows copy it to
//!   the user's other domain machines, which nobody asked for.
//! - **No `unsafe` here.** The Windows calls are made by
//!   `windows-native-keyring-store`, a thin wrapper (`docs/dependencies.md`).
//! - **Nothing of a stored value reaches an error.** The wrapper's errors
//!   can carry the bytes that were read. They are mapped to messages by
//!   kind, and such a payload is dropped unread.

use std::collections::HashMap;

use cortex_base::Secret;
use keyring_core::api::CredentialStoreApi;
use windows_native_keyring_store::Store;

use super::{CredentialError, CredentialStore, StoreKind, check_provider};
use crate::env::Env;

/// What every credential's name starts with.
const PREFIX: &str = "cortex-harness:";

/// The credential store in Windows Credential Manager.
#[derive(Debug, Clone)]
pub struct CredentialManagerStore {
    prefix: String,
}

impl Default for CredentialManagerStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialManagerStore {
    /// The store the harness uses: credentials named
    /// `cortex-harness:<provider>`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_prefix(PREFIX)
    }

    /// A store whose credentials are named `<prefix><provider>`. For tests,
    /// which use names of their own and remove them afterwards, so that no
    /// test can touch a real credential.
    #[must_use]
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    /// The name a provider's credential is stored under.
    #[must_use]
    pub fn target(&self, provider: &str) -> String {
        format!("{}{provider}", self.prefix)
    }

    fn entry(
        &self,
        provider: &str,
        action: &'static str,
    ) -> Result<keyring_core::Entry, CredentialError> {
        check_provider(provider)?;
        let target = self.target(provider);
        let modifiers = HashMap::from([("target", target.as_str()), ("persistence", "Local")]);
        Store::new()
            .and_then(|store| store.build("cortex-harness", provider, Some(&modifiers)))
            .map_err(|error| failure(action, &error))
    }
}

/// Turns the wrapper's error into ours, by kind. The variants that carry
/// what was read from the store are described and their content dropped.
fn failure(action: &'static str, error: &keyring_core::Error) -> CredentialError {
    use keyring_core::Error;
    let reason = match error {
        Error::NoStorageAccess(platform) => format!(
            "Windows would not give access to the credential store ({platform}). This happens \
             in a session with no interactive logon"
        ),
        Error::PlatformFailure(platform) => platform.to_string(),
        Error::BadEncoding(_) | Error::BadDataFormat(..) => {
            "what is stored under that name is not text Cortex-Harness wrote".to_owned()
        }
        Error::TooLong(what, limit) => {
            format!("the {what} is longer than Windows allows ({limit})")
        }
        Error::Invalid(what, why) => format!("the {what} {why}"),
        Error::NoEntry => "there is no such credential".to_owned(),
        _ => "the credential store reported an error".to_owned(),
    };
    CredentialError::Store {
        store: StoreKind::CredentialManager,
        action,
        reason,
    }
}

impl CredentialStore for CredentialManagerStore {
    fn kind(&self) -> StoreKind {
        StoreKind::CredentialManager
    }

    fn describe(&self, _: &Env) -> String {
        format!(
            "Windows Credential Manager (entries named `{}<provider>`)",
            self.prefix
        )
    }

    fn get(&self, provider: &str) -> Result<Option<Secret>, CredentialError> {
        const ACTION: &str = "read the key";
        match self.entry(provider, ACTION)?.get_password() {
            Ok(key) if key.is_empty() => Ok(None),
            Ok(key) => Ok(Some(Secret::new(key))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(failure(ACTION, &error)),
        }
    }

    fn set(&self, provider: &str, credential: &Secret) -> Result<(), CredentialError> {
        const ACTION: &str = "store the key";
        self.entry(provider, ACTION)?
            .set_password(credential.expose())
            .map_err(|error| failure(ACTION, &error))
    }

    fn remove(&self, provider: &str) -> Result<bool, CredentialError> {
        const ACTION: &str = "remove the key";
        match self.entry(provider, ACTION)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring_core::Error::NoEntry) => Ok(false),
            Err(error) => Err(failure(ACTION, &error)),
        }
    }
}
