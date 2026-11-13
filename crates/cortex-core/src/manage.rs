//! What the command line's subcommands ask of the core outside a session
//! (step 13b): the credentials of each provider, the approvals stored for a
//! project, and the project's trust.
//!
//! These are here, and not in the binary, because each one must agree with
//! what a session does: a key is looked for where a session looks for it,
//! a project is the workspace root a session would resolve, and trust is
//! pinned over the very rules a session compares with the pin
//! (`assemble::trust`). A copy of any of them in the binary could drift.
//!
//! Every function here blocks on the file system. The binary calls them from
//! its main thread, outside the core's runtime.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "these run on the binary's main thread, for a subcommand, and never on the core thread"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cortex_base::{PathOptions, PathResolver, Secret};
use cortex_config::{
    CredentialStatus, LoadOptions, Loaded, ProjectTrust as ConfigTrust, StateDir, Withheld,
};
use cortex_policy::{ApprovalStore, Definition, TrustStore};
use cortex_protocol::lease::Rule;
use cortex_protocol::methods::StoredApproval;
use cortex_protocol::{ApprovalId, ErrorKind, ProjectKey};

use crate::assemble::{project_rules, resolve_dir};
use crate::core::{Core, OpError, error};
use crate::provider::{builtin_names, key_variable};

/// A provider and where its key would come from.
#[derive(Debug)]
pub struct ProviderCredential {
    /// The provider's name.
    pub provider: String,
    /// Where its key comes from, or what went wrong asking.
    pub status: Result<CredentialStatus, String>,
}

/// A workspace as a session would see it: its root and its key.
#[derive(Debug, Clone)]
pub struct Project {
    /// The workspace root, resolved through any link.
    pub root: PathBuf,
    /// The root as a person reads it: `~/work/app`.
    pub shown: String,
    /// The key the stores file it under.
    pub key: ProjectKey,
}

/// Whether the user trusts a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    /// Not trusted.
    Untrusted,
    /// Trusted, and its rules are the ones the user approved.
    Trusted,
    /// Trusted, and its rules have changed since: what they could loosen is
    /// set aside until the change is approved.
    RulesChanged,
}

/// What trusting a project pinned.
#[derive(Debug)]
pub struct Trusted {
    /// The project's permission rules, as its files hold them now: every
    /// allow, deny and ask rule. These are what a later start compares.
    pub rules: Vec<Rule>,
    /// The project's settings that wait for trust, which now apply.
    pub settings: Vec<Withheld>,
    /// Whether the project was trusted before (its rules are approved
    /// again) or is trusted now for the first time.
    pub was_trusted: bool,
}

impl Core {
    /// Every provider the core knows (the built-in ones, then the user's
    /// own), with where its key would come from. Nothing is sent anywhere.
    #[must_use]
    pub fn credentials(&self) -> Vec<ProviderCredential> {
        let settings = &self.inner.user.settings;
        let mut names: Vec<String> = builtin_names().map(str::to_owned).collect();
        for name in settings.providers.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        let providers = &self.inner.providers;
        names
            .into_iter()
            .map(|provider| {
                let variable = key_variable(settings, &provider);
                let status = cortex_config::credentials::status(cortex_config::credentials::Lookup {
                    provider: &provider,
                    flag: providers.flags.get(&provider),
                    variable: variable.as_deref(),
                    env: &self.inner.options.env,
                    store: providers.store.as_ref(),
                })
                .map_err(|failure| failure.to_string());
                ProviderCredential { provider, status }
            })
            .collect()
    }

    /// Keeps `key` for `provider` in the credential store.
    ///
    /// # Errors
    ///
    /// Of kind `auth` when the store cannot be opened or written, or the
    /// name is not one a provider can have.
    pub fn store_credential(&self, provider: &str, key: &Secret) -> Result<(), OpError> {
        self.inner
            .providers
            .store
            .set(provider, key)
            .map_err(|failure| error(ErrorKind::Auth, "store_failed", failure.to_string()))
    }

    /// Removes the stored key of `provider`. Says whether there was one.
    ///
    /// # Errors
    ///
    /// Of kind `auth` when the store cannot be opened or written.
    pub fn remove_credential(&self, provider: &str) -> Result<bool, OpError> {
        self.inner
            .providers
            .store
            .remove(provider)
            .map_err(|failure| error(ErrorKind::Auth, "store_failed", failure.to_string()))
    }

    /// The project a session started in `directory` would be in.
    ///
    /// # Errors
    ///
    /// Of kind `usage` when the directory cannot be a workspace.
    pub fn project(&self, directory: &Path) -> Result<Project, OpError> {
        let resolver = self.resolver(directory)?;
        let root = resolver.root();
        Ok(Project {
            root: root.to_fs_path(),
            shown: self.inner.options.env.display_path(&root.to_fs_path()),
            key: root.project_key(),
        })
    }

    /// The approvals stored for a project, oldest first.
    ///
    /// # Errors
    ///
    /// When the state directory is unknown or the file cannot be read.
    pub fn stored_approvals(&self, project: &Project) -> Result<Vec<StoredApproval>, OpError> {
        self.approval_store(project)?
            .load()
            .map_err(|failure| error(ErrorKind::Internal, "store_failed", failure.to_string()))
    }

    /// Removes stored approvals of a project. Says how many there were.
    ///
    /// # Errors
    ///
    /// When the state directory is unknown or the file cannot be written.
    pub fn revoke_approvals(&self, project: &Project, ids: &[ApprovalId]) -> Result<usize, OpError> {
        self.approval_store(project)?
            .remove(ids)
            .map_err(|failure| error(ErrorKind::Internal, "store_failed", failure.to_string()))
    }

    /// Whether the user trusts a project, judged as a session judges it.
    ///
    /// # Errors
    ///
    /// When the trust store or the project's configuration cannot be read.
    pub fn project_trust(&self, project: &Project) -> Result<TrustState, OpError> {
        let stored = self
            .trust_store()?
            .project(&project.key)
            .map_err(|failure| error(ErrorKind::Internal, "store_failed", failure.to_string()))?;
        if !stored.is_trusted() {
            return Ok(TrustState::Untrusted);
        }
        let rules = project_rules(&self.load_trusted(project)?);
        Ok(if stored.rules_approved(&rules) {
            TrustState::Trusted
        } else {
            TrustState::RulesChanged
        })
    }

    /// Trusts a project, or approves the change of its rules: pins every
    /// permission rule its files hold now (allow, deny and ask, which is
    /// what a later start compares) and keeps whatever else was pinned.
    ///
    /// # Errors
    ///
    /// When the project's configuration is invalid, or the trust store
    /// cannot be read or written.
    pub fn trust_project(&self, project: &Project) -> Result<Trusted, OpError> {
        let loaded = self.load_trusted(project)?;
        let rules = project_rules(&loaded);
        // What an untrusted load holds back is what trust now turns on.
        let settings = self.load(project, ConfigTrust::Untrusted)?.withheld;
        let store = self.trust_store()?;
        let failed =
            |failure: cortex_policy::TrustError| error(ErrorKind::Internal, "store_failed", failure.to_string());
        let was_trusted = store.project(&project.key).map_err(failed)?.is_trusted();
        let pin = [Definition::of_rules(&rules)];
        if was_trusted {
            store.approve(&project.key, &pin).map_err(failed)?;
        } else {
            store.trust(&project.key, &project.shown, &pin).map_err(failed)?;
        }
        Ok(Trusted {
            rules,
            settings,
            was_trusted,
        })
    }

    /// Takes trust away from a project. Says whether it was trusted.
    ///
    /// # Errors
    ///
    /// When the trust store cannot be read or written.
    pub fn untrust_project(&self, project: &Project) -> Result<bool, OpError> {
        self.trust_store()?
            .revoke(&project.key)
            .map_err(|failure| error(ErrorKind::Internal, "store_failed", failure.to_string()))
    }

    /// Every project the user trusts, by the root it was trusted at.
    ///
    /// # Errors
    ///
    /// When the trust store cannot be read.
    pub fn trusted_projects(&self) -> Result<Vec<String>, OpError> {
        Ok(self
            .trust_store()?
            .list()
            .map_err(|failure| error(ErrorKind::Internal, "store_failed", failure.to_string()))?
            .into_iter()
            .map(|(_, root)| root)
            .collect())
    }

    /// The configuration a session in `project` would have: the project's
    /// layers read with its trust as a session judges it. With `strict`, an
    /// unknown key is an error (`cortex config check`).
    ///
    /// # Errors
    ///
    /// Of kind `config`, with every problem of every layer, when it is not
    /// valid.
    pub fn configuration(&self, project: &Project, strict: bool) -> Result<Loaded, OpError> {
        let trusted = self.inner.options.trust_project
            || self.project_trust(project)? == TrustState::Trusted;
        let trust = if trusted {
            ConfigTrust::Trusted
        } else {
            ConfigTrust::Untrusted
        };
        self.load_with(project, trust, strict)
    }

    fn resolver(&self, directory: &Path) -> Result<PathResolver, OpError> {
        let mut options = PathOptions::new();
        if let Some(home) = self.inner.options.env.home().as_deref().and_then(resolve_dir) {
            options = options.home(home);
        }
        PathResolver::new(directory, options).map_err(|failure| {
            error(
                ErrorKind::Usage,
                "invalid_workspace",
                format!("`{}` cannot be a workspace: {failure}", directory.display()),
            )
        })
    }

    fn state(&self) -> Result<StateDir, OpError> {
        self.state_dir().ok_or_else(|| {
            error(
                ErrorKind::Config,
                "no_state_dir",
                "No state directory could be worked out: the home directory is not known.",
            )
        })
    }

    fn trust_store(&self) -> Result<TrustStore, OpError> {
        Ok(TrustStore::new(
            self.state()?.trust_file(),
            Arc::clone(&self.inner.options.clock),
        ))
    }

    fn approval_store(&self, project: &Project) -> Result<ApprovalStore, OpError> {
        let file = self.state()?.approvals_file(&project.key).ok_or_else(|| {
            error(
                ErrorKind::Internal,
                "no_approvals_file",
                "The project has no place for stored approvals.",
            )
        })?;
        Ok(ApprovalStore::new(file, Arc::clone(&self.inner.options.clock)))
    }

    fn load(&self, project: &Project, trust: ConfigTrust) -> Result<Loaded, OpError> {
        self.load_with(project, trust, false)
    }

    fn load_with(
        &self,
        project: &Project,
        trust: ConfigTrust,
        strict: bool,
    ) -> Result<Loaded, OpError> {
        let options = &self.inner.options;
        let mut load = LoadOptions::new(options.env.clone());
        if let Some(dirs) = &options.dirs {
            load.dirs = dirs.clone();
        }
        load.workspace = Some(project.root.clone());
        load.profile.clone_from(&options.profile);
        load.overrides.clone_from(&options.overrides);
        load.trust = trust;
        load.strict = strict;
        cortex_config::load(&load).map_err(|failure| {
            error(
                ErrorKind::Config,
                "invalid_configuration",
                failure.render(&options.env),
            )
        })
    }

    fn load_trusted(&self, project: &Project) -> Result<Loaded, OpError> {
        self.load(project, ConfigTrust::Trusted)
    }
}
