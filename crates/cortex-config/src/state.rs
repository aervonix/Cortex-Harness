//! The state directory: where the files beside configuration live
//! (`docs/architecture/08-configuration.md`, "Other state").
//!
//! | File | Holds | Owner |
//! |---|---|---|
//! | `trust.toml` | Trusted projects and the hashes of what was trusted. | `cortex-policy` |
//! | `projects/<project key>/knowledge.toml` | What the harness has learned about a project. | `cortex-knowledge` |
//! | `projects/<project key>/index/` | The repository-map cache. | `cortex-index` |
//! | `stats/` | Local statistics. | `cortex-stats` |
//! | `approvals/<project key>.toml` | "Always allow in this project" rules. | `cortex-policy` |
//! | `catalog-overlay.toml` | Results of `cortex models refresh`. | `cortex-llm` |
//! | `history` | Prompt history for the editor. | `cortex-tui` |
//! | `logs/` | Log files and crash reports. | `cortex-core` |
//!
//! This module says **where** each file is. What is in a file is its
//! owner's business. Nothing in the state directory is needed to run:
//! deleting it loses convenience, not data.
//!
//! # Privacy
//!
//! The state directory is the user's. [`StateDir::ensure`] creates it open
//! to the user only. **A directory that already exists is not changed:** if
//! it is open to others, that is reported
//! ([`StateDir::privacy_warning`]) and not fixed, because its permissions
//! may be the user's choice. So a private file in it is written private
//! itself, with [`write_private`], and never relies on the directory.

#![allow(
    clippy::disallowed_methods,
    reason = "blocking file calls; runs at start-up and on the blocking pool"
)]

use std::io;
use std::path::{Path, PathBuf};

use cortex_base::{AtomicWriteError, AtomicWriteOptions, Clock, private, write_atomic};
use cortex_protocol::ProjectKey;

use crate::dirs::Dirs;
use crate::layer::{Diagnostic, Severity};

/// The state directory of one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDir {
    root: PathBuf,
}

/// What [`StateDir::ensure`] found or did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateReport {
    /// Whether the directory was created by this call.
    pub created: bool,
    /// Whether only the user has access to it. A directory this call
    /// created is; one that was there may not be.
    pub private: bool,
}

impl StateDir {
    /// The state directory at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The state directory the environment gives, when it gives one.
    #[must_use]
    pub fn from_dirs(dirs: &Dirs) -> Option<Self> {
        dirs.state.clone().map(Self::new)
    }

    /// The directory itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `trust.toml`: trusted projects and the hashes of what was trusted.
    #[must_use]
    pub fn trust_file(&self) -> PathBuf {
        self.root.join("trust.toml")
    }

    /// `projects/<key>/`: what is kept about one project. `None` when the
    /// key could not be a single directory name.
    #[must_use]
    pub fn project_dir(&self, project: &ProjectKey) -> Option<PathBuf> {
        Some(self.root.join("projects").join(component(project)?))
    }

    /// `projects/<key>/knowledge.toml`: what the harness has learned about
    /// a project.
    #[must_use]
    pub fn knowledge_file(&self, project: &ProjectKey) -> Option<PathBuf> {
        Some(self.project_dir(project)?.join("knowledge.toml"))
    }

    /// `projects/<key>/index/`: the repository-map cache.
    #[must_use]
    pub fn index_dir(&self, project: &ProjectKey) -> Option<PathBuf> {
        Some(self.project_dir(project)?.join("index"))
    }

    /// `approvals/<key>.toml`: the "always allow in this project" rules.
    #[must_use]
    pub fn approvals_file(&self, project: &ProjectKey) -> Option<PathBuf> {
        Some(
            self.root
                .join("approvals")
                .join(format!("{}.toml", component(project)?)),
        )
    }

    /// `stats/`: local statistics.
    #[must_use]
    pub fn stats_dir(&self) -> PathBuf {
        self.root.join("stats")
    }

    /// `catalog-overlay.toml`: the results of `cortex models refresh`.
    #[must_use]
    pub fn catalog_overlay_file(&self) -> PathBuf {
        self.root.join("catalog-overlay.toml")
    }

    /// `history`: prompt history for the editor.
    #[must_use]
    pub fn history_file(&self) -> PathBuf {
        self.root.join("history")
    }

    /// `logs/`: log files and crash reports.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Makes sure the directory exists, creating it, and any missing
    /// parents, open to the user only. A directory that is already there is
    /// left as it is, and the report says whether it is private.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created or looked at.
    pub fn ensure(&self) -> io::Result<StateReport> {
        let created = match std::fs::metadata(&self.root) {
            Ok(_) => false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        };
        private::create_dir_all(&self.root)?;
        Ok(StateReport {
            created,
            private: private::is_private(&self.root)?,
        })
    }

    /// Makes sure a directory inside the state directory exists, the same
    /// way as [`ensure`](Self::ensure).
    ///
    /// # Errors
    ///
    /// When `directory` is not inside the state directory, or cannot be
    /// created.
    pub fn ensure_dir(&self, directory: &Path) -> io::Result<()> {
        if !directory.starts_with(&self.root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the directory is not inside the state directory",
            ));
        }
        private::create_dir_all(directory)
    }

    /// A warning to show when the state directory exists and is open to
    /// other users, and `None` when it is private or not there. The
    /// directory is reported, not changed.
    #[must_use]
    pub fn privacy_warning(&self) -> Option<Diagnostic> {
        match private::is_private(&self.root) {
            Ok(false) => Some(Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: Some(self.root.clone()),
                line: None,
                key: None,
                message: "the state directory can be opened by other users of this machine. \
                          Cortex-Harness keeps each private file in it private by itself, but \
                          the names of your projects are visible. It was not changed: restrict \
                          it yourself if that is not what you want"
                    .to_owned(),
            }),
            Ok(true) | Err(_) => None,
        }
    }
}

/// Writes a state file so that only the user can open it, whatever the
/// directory's permissions are, creating the file's directory if it is
/// missing. The write is atomic: a reader sees the old file or the new one.
///
/// # Errors
///
/// When the directory cannot be created or the file cannot be written.
pub fn write_private(
    path: &Path,
    contents: &[u8],
    clock: &dyn Clock,
) -> Result<(), AtomicWriteError> {
    if let Some(directory) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        private::create_dir_all(directory).map_err(|source| AtomicWriteError::Io {
            path: path.display().to_string(),
            step: "create the directory",
            source,
        })?;
    }
    let options = AtomicWriteOptions {
        owner_only: true,
        ..AtomicWriteOptions::default()
    };
    write_atomic(path, contents, &options, clock)
}

/// A project key as one directory name. A key is "never a path", and this
/// is where that is enforced: anything that could leave the directory, or
/// that Windows would read as something else, gives `None`.
fn component(project: &ProjectKey) -> Option<&str> {
    let key = project.as_str();
    let plain = !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    plain.then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_file_has_its_place() {
        let state = StateDir::new("/state/cortex-harness");
        let project = ProjectKey::new("9f2c4a1b7d3e");
        let at = |text: &str| PathBuf::from(text);
        assert_eq!(state.root(), Path::new("/state/cortex-harness"));
        assert_eq!(state.trust_file(), at("/state/cortex-harness/trust.toml"));
        assert_eq!(
            state.knowledge_file(&project),
            Some(at(
                "/state/cortex-harness/projects/9f2c4a1b7d3e/knowledge.toml"
            ))
        );
        assert_eq!(
            state.index_dir(&project),
            Some(at("/state/cortex-harness/projects/9f2c4a1b7d3e/index"))
        );
        assert_eq!(
            state.approvals_file(&project),
            Some(at("/state/cortex-harness/approvals/9f2c4a1b7d3e.toml"))
        );
        assert_eq!(state.stats_dir(), at("/state/cortex-harness/stats"));
        assert_eq!(
            state.catalog_overlay_file(),
            at("/state/cortex-harness/catalog-overlay.toml")
        );
        assert_eq!(state.history_file(), at("/state/cortex-harness/history"));
        assert_eq!(state.logs_dir(), at("/state/cortex-harness/logs"));
    }

    #[test]
    fn a_project_key_that_could_be_a_path_has_no_place() {
        let state = StateDir::new("/state");
        for key in [
            "",
            ".",
            "..",
            "../outside",
            "a/b",
            "a\\b",
            "C:",
            "key.toml",
            "nul:",
            "with space",
            "é",
            &"k".repeat(129),
        ] {
            let key = ProjectKey::new(key);
            assert_eq!(state.project_dir(&key), None, "{key}");
            assert_eq!(state.knowledge_file(&key), None, "{key}");
            assert_eq!(state.index_dir(&key), None, "{key}");
            assert_eq!(state.approvals_file(&key), None, "{key}");
        }
        assert!(state.project_dir(&ProjectKey::new("Abc_123-x")).is_some());
        assert!(
            state
                .project_dir(&ProjectKey::new("k".repeat(128)))
                .is_some()
        );
    }

    #[test]
    fn the_state_directory_comes_from_the_environment() {
        use crate::env::{Env, Platform};
        let dirs = Dirs::from_env(&Env::empty(Platform::Unix).with("XDG_STATE_HOME", "/var/state"));
        assert_eq!(
            StateDir::from_dirs(&dirs).unwrap().root(),
            Path::new("/var/state/cortex-harness")
        );
        assert_eq!(
            StateDir::from_dirs(&Dirs::from_env(&Env::empty(Platform::Unix))),
            None
        );
    }
}
