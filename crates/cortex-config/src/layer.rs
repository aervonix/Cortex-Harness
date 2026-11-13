//! Layers, where a value came from, and what was noticed on the way.

use std::fmt;
use std::path::PathBuf;

use crate::env::Env;
use crate::key::KeyPath;
use crate::schema::TrustClass;

/// One layer of configuration, lowest priority first
/// (`docs/architecture/08-configuration.md`, "Layers").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayerKind {
    /// The defaults compiled into the program.
    Default,
    /// The managed file of a shared machine.
    Managed,
    /// The user's file.
    User,
    /// The project's committed file, `.cortex/config.toml`.
    Project,
    /// The project's local file, `.cortex/config.local.toml`.
    ProjectLocal,
    /// The selected profile.
    Profile,
    /// The `CORTEX_*` environment variables.
    Environment,
    /// The command line.
    CommandLine,
}

impl LayerKind {
    /// Every layer, lowest priority first.
    pub const ALL: [Self; 8] = [
        Self::Default,
        Self::Managed,
        Self::User,
        Self::Project,
        Self::ProjectLocal,
        Self::Profile,
        Self::Environment,
        Self::CommandLine,
    ];

    /// The layer's name, as `cortex config show --layer <name>` takes it
    /// and as an origin prints it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Managed => "managed",
            Self::User => "user",
            Self::Project => "project",
            Self::ProjectLocal => "project-local",
            Self::Profile => "profile",
            Self::Environment => "environment",
            Self::CommandLine => "command-line",
        }
    }

    /// The layer with this name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|layer| layer.as_str() == name)
    }

    /// Whether this is one of the two project files, whose values are
    /// subject to the trust rules.
    #[must_use]
    pub fn is_project(self) -> bool {
        matches!(self, Self::Project | Self::ProjectLocal)
    }
}

impl fmt::Display for LayerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where an effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The layer that set it.
    pub layer: LayerKind,
    /// For a value from a profile, the file layer that defines the profile.
    pub defined_in: Option<LayerKind>,
    /// The file, for a layer that is one.
    pub file: Option<PathBuf>,
    /// The line in the file.
    pub line: Option<u32>,
    /// The profile's name, the environment variable or the flag.
    pub detail: Option<String>,
}

impl Origin {
    /// The origin of a value nothing set.
    #[must_use]
    pub fn default_value() -> Self {
        Self {
            layer: LayerKind::Default,
            defined_in: None,
            file: None,
            line: None,
            detail: None,
        }
    }

    /// The origin as `cortex config show --origin` prints it after the `#`:
    /// `user: ~/.config/cortex-harness/config.toml:3`. Paths under the home
    /// directory are written with `~`.
    #[must_use]
    pub fn render(&self, env: &Env) -> String {
        let place = self.file.as_ref().map(|file| {
            let file = env.display_path(file);
            match self.line {
                Some(line) => format!("{file}:{line}"),
                None => file,
            }
        });
        match (self.layer, &self.detail, place) {
            (LayerKind::Profile, Some(name), Some(place)) => {
                let layer = self.defined_in.unwrap_or(LayerKind::User);
                format!("profile {name} ({layer}: {place})")
            }
            (layer, Some(detail), None) => format!("{layer}: {detail}"),
            (layer, _, Some(place)) => format!("{layer}: {place}"),
            (layer, None, None) => layer.to_string(),
        }
    }
}

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing, and nothing is wrong: a setting held back until the
    /// project is trusted, a key that is locked.
    Note,
    /// Something was ignored: an unknown key, a setting a project may not
    /// make.
    Warning,
    /// The configuration cannot be used.
    Error,
}

/// Something noticed while configuration was loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How serious it is.
    pub severity: Severity,
    /// The layer it is about.
    pub layer: Option<LayerKind>,
    /// The file.
    pub file: Option<PathBuf>,
    /// The line in the file.
    pub line: Option<u32>,
    /// The key.
    pub key: Option<KeyPath>,
    /// What was noticed, as a sentence.
    pub message: String,
}

impl Diagnostic {
    /// The diagnostic on one line: `error: <file>:<line>: <key>: <message>`.
    #[must_use]
    pub fn render(&self, env: &Env) -> String {
        let mut text = String::from(match self.severity {
            Severity::Note => "note: ",
            Severity::Warning => "warning: ",
            Severity::Error => "error: ",
        });
        match (&self.file, self.layer) {
            (Some(file), _) => {
                text.push_str(&env.display_path(file));
                if let Some(line) = self.line {
                    text.push_str(&format!(":{line}"));
                }
                text.push_str(": ");
            }
            (None, Some(layer)) if layer != LayerKind::Default => {
                text.push_str(&format!("{layer}: "));
            }
            (None, _) => {}
        }
        if let Some(key) = self.key.as_ref().filter(|key| !key.is_root()) {
            text.push_str(&format!("{key}: "));
        }
        text.push_str(&self.message);
        text
    }
}

/// A setting from a project's file that is held back until the user trusts
/// the project. What trust enables is exactly this list, so a front end can
/// show it before asking.
#[derive(Debug, Clone, PartialEq)]
pub struct Withheld {
    /// The key.
    pub key: KeyPath,
    /// Why it needs trust.
    pub class: TrustClass,
    /// The value the project gives it.
    pub value: toml::Value,
    /// Where the project sets it.
    pub origin: Origin,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::env::Platform;

    fn env() -> Env {
        Env::empty(Platform::Unix).with("HOME", "/users/someone")
    }

    #[test]
    fn layers_have_names_and_an_order() {
        for layer in LayerKind::ALL {
            assert_eq!(LayerKind::from_name(layer.as_str()), Some(layer));
        }
        assert_eq!(LayerKind::from_name("global"), None);
        assert!(LayerKind::Managed < LayerKind::User);
        assert!(LayerKind::ProjectLocal < LayerKind::Profile);
        assert!(LayerKind::Environment < LayerKind::CommandLine);
        assert!(LayerKind::Project.is_project() && LayerKind::ProjectLocal.is_project());
        assert!(!LayerKind::User.is_project() && !LayerKind::Profile.is_project());
    }

    #[test]
    fn an_origin_says_which_layer_and_where() {
        let file = Origin {
            layer: LayerKind::User,
            defined_in: None,
            file: Some(Path::new("/users/someone/.config/cortex-harness/config.toml").into()),
            line: Some(3),
            detail: None,
        };
        assert_eq!(
            file.render(&env()),
            "user: ~/.config/cortex-harness/config.toml:3"
        );
        let profile = Origin {
            layer: LayerKind::Profile,
            defined_in: Some(LayerKind::Project),
            file: Some(Path::new("/work/app/.cortex/config.toml").into()),
            line: Some(12),
            detail: Some("review".to_owned()),
        };
        assert_eq!(
            profile.render(&env()),
            "profile review (project: /work/app/.cortex/config.toml:12)"
        );
        let variable = Origin {
            layer: LayerKind::Environment,
            detail: Some("CORTEX_LOG".to_owned()),
            ..Origin::default_value()
        };
        assert_eq!(variable.render(&env()), "environment: CORTEX_LOG");
        assert_eq!(Origin::default_value().render(&env()), "default");
    }

    #[test]
    fn a_diagnostic_names_the_file_the_line_and_the_key() {
        let diagnostic = Diagnostic {
            severity: Severity::Error,
            layer: Some(LayerKind::User),
            file: Some(Path::new("/users/someone/.config/cortex-harness/config.toml").into()),
            line: Some(7),
            key: Some(KeyPath::parse("retry.attempts").unwrap()),
            message: "expected a whole number that is not negative".to_owned(),
        };
        assert_eq!(
            diagnostic.render(&env()),
            "error: ~/.config/cortex-harness/config.toml:7: retry.attempts: \
             expected a whole number that is not negative"
        );
        let flag = Diagnostic {
            severity: Severity::Warning,
            layer: Some(LayerKind::CommandLine),
            file: None,
            line: None,
            key: Some(KeyPath::parse("model.mian").unwrap()),
            message: "unknown key".to_owned(),
        };
        assert_eq!(
            flag.render(&env()),
            "warning: command-line: model.mian: unknown key"
        );
        assert!(Severity::Note < Severity::Warning && Severity::Warning < Severity::Error);
    }
}
