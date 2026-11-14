//! [`Env`]: the process's environment, taken once and passed in.
//!
//! Nothing in this crate reads an environment variable directly. Where the
//! configuration lives, which provider keys are set and what `CORTEX_*`
//! overrides say all come from an [`Env`] the caller hands over. The binary
//! builds one from the real environment at start-up. A test builds one that
//! points at a temporary directory, so no test can read the real home
//! directory by accident, and two tests cannot disturb each other through
//! process-wide state.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The operating system whose conventions apply: where files live, and
/// whether the names of environment variables ignore case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux and other Unix systems: the XDG base directories.
    Unix,
    /// Windows: `%APPDATA%`, `%LOCALAPPDATA%` and `%ProgramData%`.
    Windows,
}

impl Platform {
    /// The platform this program was built for.
    pub const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Unix
    };
}

/// A set of environment variables, and the platform whose rules read them.
///
/// **Its `Debug` form names the variables and never shows a value.** The
/// environment holds the providers' keys (`ANTHROPIC_API_KEY` and the
/// like), and an `Env` reaches the load options, the live configuration
/// and a credential lookup, any of which can end up in a log line.
#[derive(Clone)]
pub struct Env {
    platform: Platform,
    variables: BTreeMap<String, String>,
}

impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Env")
            .field("platform", &self.platform)
            .field("variables", &self.variables.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Env {
    /// The real environment of this process. A variable whose name or value
    /// is not Unicode is left out.
    #[must_use]
    pub fn from_process() -> Self {
        let mut env = Self::empty(Platform::CURRENT);
        for (name, value) in std::env::vars_os() {
            if let (Some(name), Some(value)) = (name.to_str(), value.to_str()) {
                env.set(name, value);
            }
        }
        env
    }

    /// No variables at all, under `platform`'s rules.
    #[must_use]
    pub fn empty(platform: Platform) -> Self {
        Self {
            platform,
            variables: BTreeMap::new(),
        }
    }

    /// The platform whose conventions apply.
    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Sets a variable.
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        self.variables.insert(self.name(name), value.into());
    }

    /// The same environment with one more variable. For building one in a
    /// test.
    #[must_use]
    pub fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.set(name, value);
        self
    }

    /// Removes a variable.
    pub fn remove(&mut self, name: &str) {
        let name = self.name(name);
        self.variables.remove(&name);
    }

    /// The value of a variable. One that is set to the empty string counts
    /// as not set, as it does for the XDG variables and for provider keys.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.variables
            .get(&self.name(name))
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// The value of a variable as a path, if it is an absolute one. The XDG
    /// specification says a relative path in one of its variables is to be
    /// ignored, and the same rule keeps a stray value from making the
    /// location depend on the working directory.
    #[must_use]
    pub fn absolute_path(&self, name: &str) -> Option<PathBuf> {
        let value = self.get(name)?;
        let absolute = match self.platform {
            Platform::Unix => value.starts_with('/'),
            // `C:\…`, `C:/…` or a share. Written out, so that the rule is
            // the same when the Windows conventions are tested on Linux.
            Platform::Windows => {
                let bytes = value.as_bytes();
                value.starts_with(r"\\")
                    || (bytes.len() >= 3
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && (bytes[2] == b'\\' || bytes[2] == b'/'))
            }
        };
        absolute.then(|| PathBuf::from(value))
    }

    /// The user's home directory: `HOME`, or on Windows `USERPROFILE`.
    #[must_use]
    pub fn home(&self) -> Option<PathBuf> {
        match self.platform {
            Platform::Unix => self.absolute_path("HOME"),
            Platform::Windows => self.absolute_path("USERPROFILE"),
        }
    }

    /// Writes a path with `~` for the home directory, for messages. A path
    /// outside the home directory is written whole, with `/` separators.
    #[must_use]
    pub fn display_path(&self, path: &Path) -> String {
        // By place: on Windows a path the harness resolved has the
        // extended-length prefix, and the home directory from the
        // environment does not.
        let style = match self.platform {
            Platform::Unix => cortex_base::PathStyle::Posix,
            Platform::Windows => cortex_base::PathStyle::Windows,
        };
        if let Some(home) = self.home()
            && let Some(inside) = style.strip_location_prefix(path, &home)
        {
            let inside = inside.to_string_lossy().replace('\\', "/");
            return if inside.is_empty() {
                "~".to_owned()
            } else {
                format!("~/{inside}")
            };
        }
        path.to_string_lossy().replace('\\', "/")
    }

    /// The form a name is stored under: as written on Unix, upper case on
    /// Windows, where `ProgramData` and `PROGRAMDATA` are one variable.
    fn name(&self, name: &str) -> String {
        match self.platform {
            Platform::Unix => name.to_owned(),
            Platform::Windows => name.to_ascii_uppercase(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_value_counts_as_not_set() {
        let env = Env::empty(Platform::Unix)
            .with("ANTHROPIC_API_KEY", "")
            .with("OTHER", "x");
        assert_eq!(env.get("ANTHROPIC_API_KEY"), None);
        assert_eq!(env.get("OTHER"), Some("x"));
        assert_eq!(env.get("MISSING"), None);
        let mut env = env;
        env.remove("OTHER");
        assert_eq!(env.get("OTHER"), None);
    }

    #[test]
    fn names_ignore_case_on_windows_only() {
        let windows = Env::empty(Platform::Windows).with("ProgramData", r"C:\ProgramData");
        assert_eq!(windows.get("PROGRAMDATA"), Some(r"C:\ProgramData"));
        assert_eq!(windows.get("programdata"), Some(r"C:\ProgramData"));

        let unix = Env::empty(Platform::Unix).with("Home", "/Profiles/other");
        assert_eq!(unix.get("HOME"), None);
        assert_eq!(unix.platform(), Platform::Unix);
    }

    #[test]
    fn only_an_absolute_path_is_taken_from_a_variable() {
        let unix = Env::empty(Platform::Unix)
            .with("XDG_CONFIG_HOME", "relative/config")
            .with("XDG_STATE_HOME", "/var/state");
        assert_eq!(unix.absolute_path("XDG_CONFIG_HOME"), None);
        assert_eq!(
            unix.absolute_path("XDG_STATE_HOME"),
            Some(PathBuf::from("/var/state"))
        );

        let windows = Env::empty(Platform::Windows)
            .with("A", r"C:\Data")
            .with("B", "D:/Data")
            .with("C", r"\\server\share\data")
            .with("D", r"Data\here")
            .with("E", "C:Data")
            .with("F", r"\Data");
        for (name, absolute) in [
            ("A", true),
            ("B", true),
            ("C", true),
            ("D", false),
            ("E", false),
            ("F", false),
        ] {
            assert_eq!(windows.absolute_path(name).is_some(), absolute, "{name}");
        }
    }

    #[test]
    fn a_path_under_the_home_directory_is_shown_with_a_tilde() {
        let env = Env::empty(Platform::Unix).with("HOME", "/Profiles/someone");
        assert_eq!(
            env.display_path(Path::new(
                "/Profiles/someone/.config/cortex-harness/config.toml"
            )),
            "~/.config/cortex-harness/config.toml"
        );
        assert_eq!(env.display_path(Path::new("/Profiles/someone")), "~");
        assert_eq!(
            env.display_path(Path::new("/etc/cortex-harness/config.toml")),
            "/etc/cortex-harness/config.toml"
        );
        // Another user's directory is not this user's home.
        assert_eq!(
            env.display_path(Path::new("/Profiles/someone-else/x")),
            "/Profiles/someone-else/x"
        );
        let nowhere = Env::empty(Platform::Unix);
        assert_eq!(nowhere.home(), None);
        assert_eq!(nowhere.display_path(Path::new("/a/b")), "/a/b");

        // On Windows, a path the harness resolved has the extended-length
        // prefix and the folder's own case; the variable has neither.
        let windows = Env::empty(Platform::Windows).with("USERPROFILE", r"C:\Profiles\someone");
        assert_eq!(
            windows.display_path(Path::new(r"\\?\C:\Profiles\Someone\work\app")),
            "~/work/app"
        );
        assert_eq!(
            windows.display_path(Path::new(r"\\?\C:\Profiles\someone2\x")),
            "//?/C:/Profiles/someone2/x"
        );
        assert_eq!(nowhere.display_path(Path::new("/a/b")), "/a/b");
    }

    #[test]
    fn debug_names_the_variables_and_shows_no_value() {
        let env = Env::empty(Platform::Unix)
            .with("HOME", "/Profiles/someone")
            .with("ANTHROPIC_API_KEY", "sk-test-env-0001");
        let printed = format!("{env:?} {env:#?}");
        assert!(!printed.contains("sk-test-env-0001"), "{printed}");
        assert!(!printed.contains("/Profiles/someone"), "{printed}");
        assert!(
            printed.contains("ANTHROPIC_API_KEY") && printed.contains("Unix"),
            "{printed}"
        );
    }

    #[test]
    fn the_real_environment_can_be_taken() {
        let env = Env::from_process();
        assert_eq!(env.platform(), Platform::CURRENT);
    }
}
