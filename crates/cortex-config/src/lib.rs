//! Configuration and credentials (`docs/architecture/08-configuration.md`).
//!
//! | Module | What it gives |
//! |---|---|
//! | [`schema`] | Every key of a configuration file, declared once: [`Settings`], its defaults, the trust rules, and the generated JSON Schema and reference page. |
//! | [`load`] | [`load`](load::load): find the layers, check and merge them, and return the [`Loaded`] configuration with the origin of every value. |
//! | [`reload`] | [`LiveConfig`]: notice changed files and reload, keeping the old configuration when the new one has an error. |
//! | [`credentials`] | The credential stores (an owner-only file, Windows Credential Manager) and the lookup order: flag, environment variable, store. |
//! | [`state`] | [`StateDir`]: where the state files live. |
//! | [`discover`] | What other tools left on the machine that could be imported. Reported, never read. |
//! | [`env`], [`dirs`] | The environment and the directories, passed in, never read from the process behind the caller's back. |
//!
//! # What this crate does not do
//!
//! - **It holds no secret.** No key of the configuration file takes one. A
//!   credential is a [`cortex_base::Secret`] from the moment it is read.
//! - **It does not decide trust.** Whether a project is trusted is an input
//!   ([`ProjectTrust`]). The trust store and its hashes belong to
//!   `cortex-policy`.
//! - **It does not know the model catalog.** A model name or a reasoning
//!   level is checked for shape here and against the catalog by the crate
//!   that has one.
//! - **It is not the `AuthMethod` layer.** That trait is in `cortex-llm`,
//!   which this crate may not depend on. This crate finds the key and says
//!   where it came from; the core hands it to the provider layer.
//!
//! # Blocking
//!
//! Loading, reloading and the credential stores read and write files, and
//! block. They run at start-up and on the blocking pool.

pub mod credentials;
pub mod dirs;
pub mod discover;
pub mod duration;
pub mod env;
pub mod key;
pub mod layer;
pub mod load;
pub mod reload;
pub mod schema;
pub mod state;

mod document;
mod merge;
mod validate;

pub use credentials::{
    Credential, CredentialError, CredentialSource, CredentialStatus, CredentialStore, FileStore,
    StoreKind, open_store, resolve,
};
pub use dirs::{Dirs, ProjectFiles, workspace_root};
pub use env::{Env, Platform};
pub use key::KeyPath;
pub use layer::{Diagnostic, LayerKind, Origin, Severity, Withheld};
pub use load::{
    Change, LoadFailure, LoadOptions, Loaded, Override, PlacedRule, ProjectTrust, load,
};
pub use reload::{LiveConfig, Reload};
pub use schema::{Effect, Settings, Trust, TrustClass};
pub use state::StateDir;
