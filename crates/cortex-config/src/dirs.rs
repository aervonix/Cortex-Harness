//! Where Cortex-Harness keeps its files (`docs/architecture/08-configuration.md`).
//!
//! | What | Linux | Windows |
//! |---|---|---|
//! | Configuration | `$XDG_CONFIG_HOME/cortex-harness`, usually `~/.config/cortex-harness` | `%APPDATA%\cortex-harness` |
//! | State | `$XDG_STATE_HOME/cortex-harness`, usually `~/.local/state/cortex-harness` | `%LOCALAPPDATA%\cortex-harness\state` |
//! | Data (sessions) | `$XDG_DATA_HOME/cortex-harness`, usually `~/.local/share/cortex-harness` | `%LOCALAPPDATA%\cortex-harness` |
//! | Managed configuration | `/etc/cortex-harness/config.toml` | `%ProgramData%\cortex-harness\config.toml` |
//!
//! Two variables of our own move things: `CORTEX_CONFIG` names the user's
//! configuration file, and `CORTEX_DATA_DIR` the data directory.
//!
//! Everything is worked out from an [`Env`], never from the process's real
//! environment, and nothing here touches the file system.

use std::path::{Path, PathBuf};

use crate::env::{Env, Platform};

/// The folder name used under every base directory.
pub const APP_DIR: &str = "cortex-harness";
/// The name of a configuration file, in the user's directory and in a
/// project's `.cortex` directory.
pub const CONFIG_FILE: &str = "config.toml";
/// The name of a project's local, uncommitted configuration file.
pub const LOCAL_CONFIG_FILE: &str = "config.local.toml";
/// The directory in a workspace that holds the project's configuration.
pub const PROJECT_DIR: &str = ".cortex";
/// The name of the credentials file, in the configuration directory.
pub const CREDENTIALS_FILE: &str = "credentials.toml";

/// The directories and files of one user on one machine. A location is
/// `None` when the environment does not say where it is (no home directory),
/// and whatever would live there is then simply absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// The user's home directory.
    pub home: Option<PathBuf>,
    /// The configuration directory. The credentials file lives here.
    pub config: Option<PathBuf>,
    /// The user's configuration file: `config.toml` in the configuration
    /// directory, unless `CORTEX_CONFIG` names another file.
    pub user_config: Option<PathBuf>,
    /// The managed configuration file, for shared machines.
    pub managed_config: Option<PathBuf>,
    /// The state directory: trust, approvals, caches, logs. Nothing in it
    /// is needed to run.
    pub state: Option<PathBuf>,
    /// The data directory, where sessions are stored, before configuration
    /// has had its say (`session.data_dir`).
    pub data: Option<PathBuf>,
}

impl Dirs {
    /// Works the locations out from the environment.
    #[must_use]
    pub fn from_env(env: &Env) -> Self {
        let home = env.home();
        let under_home = |parts: &[&str]| {
            home.as_ref().map(|home| {
                let mut path = home.clone();
                path.extend(parts);
                path
            })
        };
        let app = |base: Option<PathBuf>| base.map(|base| base.join(APP_DIR));

        let (config, state, data, managed_config) = match env.platform() {
            Platform::Unix => (
                app(env
                    .absolute_path("XDG_CONFIG_HOME")
                    .or_else(|| under_home(&[".config"]))),
                app(env
                    .absolute_path("XDG_STATE_HOME")
                    .or_else(|| under_home(&[".local", "state"]))),
                app(env
                    .absolute_path("XDG_DATA_HOME")
                    .or_else(|| under_home(&[".local", "share"]))),
                Some(PathBuf::from("/etc").join(APP_DIR).join(CONFIG_FILE)),
            ),
            Platform::Windows => {
                let local = app(env
                    .absolute_path("LOCALAPPDATA")
                    .or_else(|| under_home(&["AppData", "Local"])));
                (
                    app(env
                        .absolute_path("APPDATA")
                        .or_else(|| under_home(&["AppData", "Roaming"]))),
                    local.as_ref().map(|local| local.join("state")),
                    local,
                    app(env.absolute_path("ProgramData")).map(|dir| dir.join(CONFIG_FILE)),
                )
            }
        };

        Self {
            user_config: env
                .absolute_path("CORTEX_CONFIG")
                .or_else(|| config.as_ref().map(|dir| dir.join(CONFIG_FILE))),
            data: env.absolute_path("CORTEX_DATA_DIR").or(data),
            home,
            config,
            managed_config,
            state,
        }
    }

    /// The credentials file: `credentials.toml` in the configuration
    /// directory.
    #[must_use]
    pub fn credentials_file(&self) -> Option<PathBuf> {
        self.config.as_ref().map(|dir| dir.join(CREDENTIALS_FILE))
    }
}

/// The two configuration files of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFiles {
    /// The workspace root.
    pub root: PathBuf,
    /// `<root>/.cortex/config.toml`, committed with the repository.
    pub config: PathBuf,
    /// `<root>/.cortex/config.local.toml`, not committed.
    pub local_config: PathBuf,
}

impl ProjectFiles {
    /// The configuration files of the project whose root is `root`.
    #[must_use]
    pub fn at(root: &Path) -> Self {
        let directory = root.join(PROJECT_DIR);
        Self {
            root: root.to_path_buf(),
            config: directory.join(CONFIG_FILE),
            local_config: directory.join(LOCAL_CONFIG_FILE),
        }
    }
}

/// The workspace root for a working directory: the nearest directory, from
/// `start` upwards, that contains `.cortex` or `.git`, or `start` itself
/// when none does.
///
/// This looks at the file system (one or two `stat` calls a level) and
/// blocks.
#[must_use]
pub fn workspace_root(start: &Path) -> PathBuf {
    #![allow(
        clippy::disallowed_methods,
        reason = "a stat call per level, at start-up, before any session exists"
    )]
    start
        .ancestors()
        .find(|directory| {
            std::fs::symlink_metadata(directory.join(PROJECT_DIR)).is_ok_and(|found| found.is_dir())
                // `.git` is a directory in a repository and a file in a
                // worktree or a submodule.
                || std::fs::symlink_metadata(directory.join(".git")).is_ok()
        })
        .unwrap_or(start)
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_follows_the_xdg_directories() {
        let env = Env::empty(Platform::Unix).with("HOME", "/users/someone");
        let dirs = Dirs::from_env(&env);
        let path = |text: &str| Some(PathBuf::from(text));
        assert_eq!(dirs.home, path("/users/someone"));
        assert_eq!(dirs.config, path("/users/someone/.config/cortex-harness"));
        assert_eq!(
            dirs.user_config,
            path("/users/someone/.config/cortex-harness/config.toml")
        );
        assert_eq!(
            dirs.state,
            path("/users/someone/.local/state/cortex-harness")
        );
        assert_eq!(
            dirs.data,
            path("/users/someone/.local/share/cortex-harness")
        );
        assert_eq!(dirs.managed_config, path("/etc/cortex-harness/config.toml"));
        assert_eq!(
            dirs.credentials_file(),
            path("/users/someone/.config/cortex-harness/credentials.toml")
        );

        let env = env
            .with("XDG_CONFIG_HOME", "/cfg")
            .with("XDG_STATE_HOME", "/state")
            .with("XDG_DATA_HOME", "relative/ignored");
        let dirs = Dirs::from_env(&env);
        assert_eq!(dirs.config, path("/cfg/cortex-harness"));
        assert_eq!(dirs.state, path("/state/cortex-harness"));
        assert_eq!(
            dirs.data,
            path("/users/someone/.local/share/cortex-harness")
        );
    }

    #[test]
    fn windows_follows_its_own_folders() {
        let env = Env::empty(Platform::Windows)
            .with("USERPROFILE", r"C:\People\someone")
            .with("APPDATA", r"C:\People\someone\AppData\Roaming")
            .with("LOCALAPPDATA", r"C:\People\someone\AppData\Local")
            .with("ProgramData", r"C:\ProgramData");
        let dirs = Dirs::from_env(&env);
        let roaming = PathBuf::from(r"C:\People\someone\AppData\Roaming").join("cortex-harness");
        let local = PathBuf::from(r"C:\People\someone\AppData\Local").join("cortex-harness");
        assert_eq!(dirs.config, Some(roaming.clone()));
        assert_eq!(dirs.user_config, Some(roaming.join("config.toml")));
        assert_eq!(dirs.state, Some(local.join("state")));
        assert_eq!(dirs.data, Some(local));
        assert_eq!(
            dirs.managed_config,
            Some(
                PathBuf::from(r"C:\ProgramData")
                    .join("cortex-harness")
                    .join("config.toml")
            )
        );

        // Without the two variables the usual places under the profile are
        // used, and without `ProgramData` there is no managed file.
        let bare = Dirs::from_env(
            &Env::empty(Platform::Windows).with("USERPROFILE", r"C:\People\someone"),
        );
        let mut expected = PathBuf::from(r"C:\People\someone");
        expected.extend(["AppData", "Roaming", "cortex-harness"]);
        assert_eq!(bare.config, Some(expected));
        assert_eq!(bare.managed_config, None);
    }

    #[test]
    fn our_own_variables_move_the_user_file_and_the_data_directory() {
        let env = Env::empty(Platform::Unix)
            .with("HOME", "/users/someone")
            .with("CORTEX_CONFIG", "/elsewhere/my.toml")
            .with("CORTEX_DATA_DIR", "/big-disk/cortex");
        let dirs = Dirs::from_env(&env);
        assert_eq!(dirs.user_config, Some(PathBuf::from("/elsewhere/my.toml")));
        assert_eq!(dirs.data, Some(PathBuf::from("/big-disk/cortex")));
        // The credentials file stays in the configuration directory.
        assert_eq!(
            dirs.credentials_file(),
            Some(PathBuf::from(
                "/users/someone/.config/cortex-harness/credentials.toml"
            ))
        );
    }

    #[test]
    fn with_no_home_directory_there_is_no_user_location() {
        let dirs = Dirs::from_env(&Env::empty(Platform::Unix));
        assert_eq!(
            (
                &dirs.home,
                &dirs.config,
                &dirs.user_config,
                &dirs.state,
                &dirs.data
            ),
            (&None, &None, &None, &None, &None)
        );
        assert!(dirs.credentials_file().is_none());
        assert!(dirs.managed_config.is_some());
    }

    #[test]
    fn a_project_has_two_files() {
        let files = ProjectFiles::at(Path::new("/work/project"));
        assert_eq!(files.root, Path::new("/work/project"));
        assert_eq!(files.config, Path::new("/work/project/.cortex/config.toml"));
        assert_eq!(
            files.local_config,
            Path::new("/work/project/.cortex/config.local.toml")
        );
    }
}
