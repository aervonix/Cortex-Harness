//! Where Cortex-Harness keeps its files (`docs/architecture/08-configuration.md`).
//!
//! | What | Linux | Windows |
//! |---|---|---|
//! | Configuration | `$XDG_CONFIG_HOME/cortex-harness`, usually `~/.config/cortex-harness` | `%APPDATA%\cortex-harness` |
//! | State | `$XDG_STATE_HOME/cortex-harness`, usually `~/.local/state/cortex-harness` | `%LOCALAPPDATA%\cortex-harness\state` |
//! | Data (sessions) | `$XDG_DATA_HOME/cortex-harness`, usually `~/.local/share/cortex-harness` | `%LOCALAPPDATA%\cortex-harness` |
//! | Managed configuration | `/etc/cortex-harness/config.toml` | `%ProgramData%\cortex-harness\config.toml` |
//!
//! One variable of our own moves a file here: `CORTEX_CONFIG` names the
//! user's configuration file. `CORTEX_DATA_DIR` is not read here: it is
//! the environment layer's `session.data_dir`, so that a managed lock on
//! that key holds against it, and [`Dirs::data`] is the platform's place.
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
/// The name of the credentials file, in [`Dirs::credentials`].
pub const CREDENTIALS_FILE: &str = "credentials.toml";

/// The directories and files of one user on one machine. A location is
/// `None` when the environment does not say where it is (no home directory),
/// and whatever would live there is then simply absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// The user's home directory.
    pub home: Option<PathBuf>,
    /// The configuration directory.
    pub config: Option<PathBuf>,
    /// Where the credentials file lives: the configuration directory on
    /// Linux, and on Windows the local application data folder
    /// (`%LOCALAPPDATA%\cortex-harness`), not the roaming one beside the
    /// configuration. A stored key stays on its machine (decision 0018),
    /// and Windows copies a roaming profile to every machine the user signs
    /// in to.
    pub credentials: Option<PathBuf>,
    /// The user's configuration file: `config.toml` in the configuration
    /// directory, unless `CORTEX_CONFIG` names another file.
    pub user_config: Option<PathBuf>,
    /// The managed configuration file, for shared machines.
    pub managed_config: Option<PathBuf>,
    /// The state directory: trust, approvals, caches, logs. Nothing in it
    /// is needed to run.
    pub state: Option<PathBuf>,
    /// The platform's data directory, where sessions are stored unless
    /// configuration says otherwise. `session.data_dir`, which the
    /// `CORTEX_DATA_DIR` variable sets, is the other way to it:
    /// [`Loaded::data_dir`](crate::Loaded::data_dir) gives the answer.
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

        let (config, state, data, managed_config, credentials) = match env.platform() {
            Platform::Unix => {
                let config = app(env
                    .absolute_path("XDG_CONFIG_HOME")
                    .or_else(|| under_home(&[".config"])));
                (
                    config.clone(),
                    app(env
                        .absolute_path("XDG_STATE_HOME")
                        .or_else(|| under_home(&[".local", "state"]))),
                    app(env
                        .absolute_path("XDG_DATA_HOME")
                        .or_else(|| under_home(&[".local", "share"]))),
                    Some(PathBuf::from("/etc").join(APP_DIR).join(CONFIG_FILE)),
                    config,
                )
            }
            Platform::Windows => {
                let local = app(env
                    .absolute_path("LOCALAPPDATA")
                    .or_else(|| under_home(&["AppData", "Local"])));
                (
                    app(env
                        .absolute_path("APPDATA")
                        .or_else(|| under_home(&["AppData", "Roaming"]))),
                    local.as_ref().map(|local| local.join("state")),
                    local.clone(),
                    app(env.absolute_path("ProgramData")).map(|dir| dir.join(CONFIG_FILE)),
                    local,
                )
            }
        };

        Self {
            user_config: env
                .absolute_path("CORTEX_CONFIG")
                .or_else(|| config.as_ref().map(|dir| dir.join(CONFIG_FILE))),
            // Not `CORTEX_DATA_DIR`: that variable is the environment layer's
            // `session.data_dir`, which a managed lock holds. Read here as
            // well, it would move a data directory the lock keeps in place.
            data,
            home,
            config,
            credentials,
            managed_config,
            state,
        }
    }

    /// The credentials file: `credentials.toml` in
    /// [`credentials`](Self::credentials).
    #[must_use]
    pub fn credentials_file(&self) -> Option<PathBuf> {
        self.credentials
            .as_ref()
            .map(|dir| dir.join(CREDENTIALS_FILE))
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

/// Finds what is in a workspace, before the project is trusted, without
/// following a link anywhere the rules have not seen first.
///
/// A repository can put a link where a file of its own is looked for. On
/// Windows, a link to a network share makes the system send the account's
/// credentials to the server it names as soon as the link is followed, by
/// opening the file or merely asking about it (`docs/security.md`, section
/// 7). So a project's files are found through
/// [`cortex_base::PathResolver`], which follows each link itself and
/// refuses a share before the network is asked. A workspace that is itself
/// on a share is where the user chose to work, and its own share is
/// allowed; a link to any other share is refused.
#[derive(Debug)]
pub(crate) struct ProjectLocator {
    root: PathBuf,
    /// `Ok(None)`: there is no workspace directory, so nothing is in it.
    /// `Err`: why the workspace itself could not be resolved.
    resolver: Result<Option<cortex_base::PathResolver>, String>,
}

impl ProjectLocator {
    /// A locator for the workspace at `root`. This resolves the root, a
    /// `stat` call or two a level, and blocks.
    pub(crate) fn new(root: &Path) -> Self {
        use cortex_base::{PathError, PathOptions, PathResolver};
        let resolver = match PathResolver::new(root, PathOptions::new()) {
            Ok(resolver) => Ok(Some(resolver)),
            Err(PathError::ShareNotAllowed { server, share, .. }) => {
                PathResolver::new(root, PathOptions::new().allow_share(server, share)).map(Some)
            }
            Err(PathError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(error) => Err(error),
        };
        Self {
            root: root.to_path_buf(),
            resolver: resolver.map_err(|error| error.to_string()),
        }
    }

    /// Where `path`, inside the workspace, really is: every link on the way
    /// followed and checked. `Ok(None)` when there is no workspace
    /// directory. The location may not exist.
    ///
    /// # Errors
    ///
    /// When the way there is refused: a link to a network share, a link to
    /// nothing, a name that is not a file name. The message says which.
    pub(crate) fn locate(&self, path: &Path) -> Result<Option<PathBuf>, String> {
        let resolver = match &self.resolver {
            Ok(Some(resolver)) => resolver,
            Ok(None) => return Ok(None),
            Err(error) => return Err(error.clone()),
        };
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        let Some(relative) = relative.to_str() else {
            return Err(format!(
                "`{}` is not valid Unicode, so it cannot be checked",
                relative.to_string_lossy()
            ));
        };
        resolver
            .resolve(relative)
            .map(|resolved| Some(resolved.to_fs_path()))
            .map_err(|error| error.to_string())
    }
}

/// The workspace root for a working directory: the nearest directory, from
/// `start` upwards, that contains a `.cortex` folder or a `.git` that the
/// current user owns, or `start` itself when none does.
///
/// **Whose it is matters.** Anyone can make a folder in `C:\` or in a
/// shared temporary directory. Without the check, a `.git` or `.cortex`
/// that another account put there would make every directory below it one
/// workspace, whose project files that account writes (the class of git's
/// CVE-2022-24765). So a candidate counts only when it is the user's:
///
/// - **Unix:** owned by this process's effective user. The entry itself is
///   asked, not what a link there points to. The user is read from
///   `/proc/self/status`; where that cannot be read, no candidate counts
///   and the root is `start`.
/// - **Windows:** owned by the current user, or by the Administrators group
///   (`S-1-5-32-544`), which is what owns what an elevated process creates.
///   Only an administrator can create an object that group owns, and an
///   administrator can change the user's own files already. A link or a
///   junction is skipped: its owner cannot be read without following it,
///   and following a link can reach a network share.
///
/// This looks at the file system (one or two `stat` calls a level, and on
/// Windows a read of the owner of each candidate found) and blocks.
#[must_use]
pub fn workspace_root(start: &Path) -> PathBuf {
    find_workspace_root(start).root
}

/// The workspace root, and what was passed over on the way to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot {
    /// The workspace root, as [`workspace_root`] gives it.
    pub root: PathBuf,
    /// Each `.cortex` or `.git` nearer than the root that was not counted,
    /// because the user does not own it or it is a link on Windows. Worth
    /// telling the user, who may expect one of them to be the project.
    pub passed_over: Vec<PathBuf>,
}

/// [`workspace_root`], with the candidates it passed over.
#[must_use]
pub fn find_workspace_root(start: &Path) -> WorkspaceRoot {
    find_root(start, &CurrentUser::of_this_process())
}

fn find_root(start: &Path, user: &CurrentUser) -> WorkspaceRoot {
    #![allow(
        clippy::disallowed_methods,
        reason = "a stat call per level, at start-up, before any session exists"
    )]
    let mut passed_over = Vec::new();
    let mut ours = |candidate: PathBuf, folder_only: bool| {
        let Ok(found) = std::fs::symlink_metadata(&candidate) else {
            return false;
        };
        // A `.cortex` that is not a folder is not a project's folder at all.
        if folder_only && !found.is_dir() && !found.file_type().is_symlink() {
            return false;
        }
        let counts = (!folder_only || found.is_dir()) && user.owns(&candidate, &found);
        if !counts {
            passed_over.push(candidate);
        }
        counts
    };
    let root = start
        .ancestors()
        .find(|directory| {
            ours(directory.join(PROJECT_DIR), true)
                // `.git` is a directory in a repository and a file in a
                // worktree or a submodule.
                || ours(directory.join(".git"), false)
        })
        .unwrap_or(start)
        .to_path_buf();
    WorkspaceRoot { root, passed_over }
}

/// The user this process runs as, as the file system records owners.
struct CurrentUser {
    /// The effective user id, when it could be read.
    #[cfg(unix)]
    uid: Option<u32>,
}

impl CurrentUser {
    fn of_this_process() -> Self {
        Self {
            #[cfg(unix)]
            uid: effective_uid(),
        }
    }

    /// Whether the user owns the entry at `path`, which `found` describes
    /// without following a link.
    #[cfg(unix)]
    fn owns(&self, _path: &Path, found: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        self.uid == Some(found.uid())
    }

    #[cfg(windows)]
    fn owns(&self, path: &Path, found: &std::fs::Metadata) -> bool {
        // `access_list` follows a link, so a link is never asked.
        !found.file_type().is_symlink()
            && cortex_base::private::access_list(path)
                .is_ok_and(|list| owner_accepted(&list.owner, list.owned_by_current_user))
    }
}

/// Whether an object with this owner, a security identifier in text,
/// counts as the user's on Windows: the user's own, or the Administrators
/// group's.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "the rule is tested on every OS")
)]
fn owner_accepted(owner: &str, owner_is_current_user: bool) -> bool {
    /// The Administrators group.
    const ADMINISTRATORS: &str = "S-1-5-32-544";
    owner_is_current_user || owner.eq_ignore_ascii_case(ADMINISTRATORS)
}

/// This process's effective user id, read once from `/proc/self/status`.
/// The standard library has no safe way to ask, and this crate has no
/// `unsafe`. `None` where there is no such file: on a Unix other than
/// Linux, or with `/proc` not mounted.
#[cfg(unix)]
fn effective_uid() -> Option<u32> {
    #![allow(
        clippy::disallowed_methods,
        reason = "one small read, once, at start-up"
    )]
    static UID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *UID.get_or_init(|| {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        effective_uid_in(&status)
    })
}

/// The effective user id in the text of `/proc/<pid>/status`: the second of
/// the four numbers on its `Uid:` line (real, effective, saved, file
/// system).
#[cfg(unix)]
fn effective_uid_in(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
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
        // A stored key stays on its machine: the credentials file is in the
        // local folder, which Windows does not copy to the user's other
        // machines, not in the roaming one beside the configuration.
        assert_eq!(
            dirs.credentials_file(),
            Some(local.join("credentials.toml"))
        );
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
        let mut expected = PathBuf::from(r"C:\People\someone");
        expected.extend(["AppData", "Local", "cortex-harness"]);
        assert_eq!(bare.credentials, Some(expected));
        assert_eq!(bare.managed_config, None);
    }

    #[test]
    fn our_own_variable_moves_the_user_file_and_not_the_data_directory() {
        let env = Env::empty(Platform::Unix)
            .with("HOME", "/users/someone")
            .with("CORTEX_CONFIG", "/elsewhere/my.toml")
            .with("CORTEX_DATA_DIR", "/big-disk/cortex");
        let dirs = Dirs::from_env(&env);
        assert_eq!(dirs.user_config, Some(PathBuf::from("/elsewhere/my.toml")));
        // The data directory variable is a setting, `session.data_dir`, in
        // the environment layer, where a managed lock holds against it.
        assert_eq!(
            dirs.data,
            Some(PathBuf::from("/users/someone/.local/share/cortex-harness"))
        );
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
    fn on_windows_the_user_or_the_administrators_own_a_candidate() {
        assert!(owner_accepted("S-1-5-21-1-2-3-1001", true));
        assert!(owner_accepted("S-1-5-32-544", false));
        assert!(owner_accepted("s-1-5-32-544", false));
        // Another account, SYSTEM, the Users group, everyone, nobody.
        for other in [
            "S-1-5-21-1-2-3-1002",
            "S-1-5-18",
            "S-1-5-32-545",
            "S-1-1-0",
            "S-1-5-32-5440",
            "",
        ] {
            assert!(!owner_accepted(other, false), "{other:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_effective_user_is_the_second_number_on_the_uid_line() {
        let status = "Name:\tcortex\nUmask:\t0022\nUid:\t1000\t1001\t1002\t1003\n\
                      Gid:\t100\t100\t100\t100\n";
        assert_eq!(effective_uid_in(status), Some(1001));
        for wrong in ["Name:\tx\n", "Uid:\t1000\n", "Uid:\tx\ty\n", ""] {
            assert_eq!(effective_uid_in(wrong), None, "{wrong:?}");
        }
    }

    /// The real file system, with the user who owns a candidate made up:
    /// no test can make a folder that another user owns.
    #[cfg(unix)]
    #[test]
    fn on_unix_a_candidate_another_user_owns_is_passed_over() {
        #![allow(
            clippy::disallowed_methods,
            reason = "a test of the file system; a folder of its own"
        )]
        use std::os::unix::fs::MetadataExt;

        let base =
            std::env::temp_dir().join(format!("cortex-config-root-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ws").join(".git")).unwrap();
        std::fs::create_dir_all(base.join("ws").join("sub")).unwrap();
        let start = base.join("ws").join("sub");
        let mine = std::fs::symlink_metadata(base.join("ws").join(".git"))
            .unwrap()
            .uid();
        if cfg!(target_os = "linux") {
            assert_eq!(effective_uid(), Some(mine), "read from /proc/self/status");
        }

        let user = |uid| CurrentUser { uid };
        let mine = user(Some(mine));
        let other = user(mine.uid.map(|uid| uid.wrapping_add(1)));
        assert_eq!(find_root(&start, &mine).root, base.join("ws"));
        assert!(find_root(&start, &mine).passed_over.is_empty());
        // The same `.git`, as another user would see it: passed over, and
        // said so.
        let theirs = find_root(&start, &other);
        assert_ne!(theirs.root, base.join("ws"));
        assert_eq!(theirs.passed_over[0], base.join("ws").join(".git"));
        // Without a user to compare with, nothing counts.
        assert_eq!(find_root(&start, &user(None)).root, start);

        // The same for a `.cortex` folder.
        std::fs::create_dir_all(base.join("project").join(".cortex")).unwrap();
        std::fs::create_dir_all(base.join("project").join("sub")).unwrap();
        let start = base.join("project").join("sub");
        assert_eq!(find_root(&start, &mine).root, base.join("project"));
        assert_ne!(find_root(&start, &other).root, base.join("project"));

        // A link counts by its own owner, not its target's: one that leads
        // nowhere is still the user's entry, and an owner read through it
        // would find nothing.
        std::fs::create_dir_all(base.join("linked").join("sub")).unwrap();
        std::os::unix::fs::symlink(base.join("nowhere"), base.join("linked").join(".git")).unwrap();
        let start = base.join("linked").join("sub");
        assert_eq!(find_root(&start, &mine).root, base.join("linked"));
        assert_ne!(find_root(&start, &other).root, base.join("linked"));
        let _ = std::fs::remove_dir_all(&base);
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
