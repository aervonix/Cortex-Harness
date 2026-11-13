//! Project trust, pinned by hash
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Project trust").
//!
//! A repository is not trusted until the user says so, and then only for
//! the definitions they saw. What needs trust is whatever in a project can
//! run code or grant permissions: its rules, hooks, extensions, MCP
//! servers, and the learned or shared commands used for the safe list.
//!
//! **Trust is pinned.** When the user trusts a project, the hash of each
//! definition shown to them is stored with the trust. Afterwards a
//! definition stands in one of four ways ([`Standing`]):
//!
//! | Standing | Meaning | Used? |
//! |---|---|---|
//! | `Approved` | Its hash is the one the user approved. | Yes. |
//! | `Changed` | It was approved, and its content has changed since. | No, until the user approves the change. |
//! | `New` | The project is trusted, and this definition was not there to see. | No, until approved. |
//! | `Untrusted` | The project is not trusted. | No. |
//!
//! That closes "approved once, changed later": a hook that was harmless
//! when the user looked is not run after someone edits it.
//!
//! # The stored file
//!
//! `trust.toml` in the state directory:
//!
//! ```toml
//! version = 1
//!
//! [[project]]
//! key = "9f2c4a1b7d3e…"
//! root = "~/work/app"
//! trusted_at = "2026-11-13T10:15:02Z"
//!
//! [[project.pinned]]
//! kind = "hook"
//! name = "format"
//! hash = "5e884898da28…"
//! ```
//!
//! - **Projects are known by their key** (`AbsPath::project_key`), which is
//!   made from the resolved root in its stored case, so a folder that
//!   differs from a trusted one only in case is another project. `root` is
//!   for showing a person which project an entry is, and is never compared.
//! - **Written owner-only, atomically, on every write,** under a lock held
//!   from the read to the write, so that two sessions that change it at
//!   once both keep their change: a `revoke` is never undone by a `trust`
//!   that read the file before it (`crate::store_lock`).
//! - **Read strictly.** A key this build does not know, or a newer
//!   `version`, refuses the whole file, and then **nothing is trusted**: a
//!   file that cannot be read in full must not be read in part.

#![allow(
    clippy::disallowed_methods,
    reason = "blocking file calls; the store is used at start-up and on the blocking pool"
)]

use std::io;
use std::path::{Path, PathBuf};

use cortex_base::{
    AtomicWriteError, AtomicWriteOptions, ContentHash, ContentHasher, SharedClock, write_atomic,
};
use cortex_protocol::ids::{ProjectKey, Timestamp};
use cortex_protocol::lease::Rule;
use cortex_protocol::methods::TrustResourceKind;
use serde::{Deserialize, Serialize};

use crate::store_lock;

/// The version of the file's format this build writes and reads.
const VERSION: u32 = 1;

/// The largest file read.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// One thing in a project that can run code or grant permissions, as it is
/// now: what kind of thing, its name, and the hash of its definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// What kind of thing it is.
    pub kind: TrustResourceKind,
    /// Its name, unique within its kind: a hook's or a server's configured
    /// name.
    pub name: String,
    /// The hash of everything that decides what it does.
    pub hash: ContentHash,
}

impl Definition {
    /// A definition whose content is these parts, in this order: a hook's
    /// command and each argument; an extension's manifest and its script;
    /// an MCP server's command, arguments and each tool definition.
    ///
    /// The parts are hashed with their lengths, so that moving a character
    /// from one part to the next is a different definition: `["rm", "-rf
    /// x"]` is not `["rm -rf", "x"]`.
    #[must_use]
    pub fn of<'a>(
        kind: TrustResourceKind,
        name: impl Into<String>,
        parts: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut hasher = ContentHasher::new();
        for part in parts {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        Self {
            kind,
            name: name.into(),
            hash: hasher.finish(),
        }
    }

    /// The definition of a project's rules, taken together: all of them,
    /// the ones that deny and ask as well as the ones that allow. The order
    /// of the rules does not matter to what they mean, so it does not
    /// matter to the hash, and neither does a rule written twice.
    ///
    /// The rules that deny and ask take effect without trust, and are
    /// pinned all the same: what the user trusted was the allow rules *with*
    /// what held them back. A project that drops `deny exec make deploy`
    /// beside `allow exec make *` has loosened, and must be approved again
    /// (found by the review of step 12).
    #[must_use]
    pub fn of_rules(rules: &[Rule]) -> Self {
        let mut lines: Vec<String> = rules
            .iter()
            .map(|rule| {
                format!(
                    "{}\u{0}{}\u{0}{}",
                    rule.action.as_str(),
                    rule.effect.as_str(),
                    rule.pattern
                )
            })
            .collect();
        lines.sort();
        lines.dedup();
        Self::of(
            TrustResourceKind::Rules,
            "permissions.rules",
            lines.iter().map(String::as_str),
        )
    }
}

/// How a definition stands with the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Standing {
    /// The user approved exactly this.
    Approved,
    /// The user approved it, and it has changed since. Disabled until they
    /// approve the change.
    Changed,
    /// The project is trusted, and this was not among what the user saw.
    New,
    /// The project is not trusted.
    Untrusted,
}

impl Standing {
    /// Whether the definition may be used.
    #[must_use]
    pub fn is_approved(self) -> bool {
        self == Self::Approved
    }
}

/// Why the trust store could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// The file could not be read.
    #[error("the trust store `{path}` could not be read, so no project is trusted: {source}")]
    Read {
        /// The file.
        path: String,
        /// Why.
        source: io::Error,
    },
    /// The file is not one this build understands in full. Nothing in it
    /// is trusted, and it is left as it is.
    #[error(
        "the trust store `{path}` is not understood, so no project is trusted: {problem}. The \
         file was left as it is"
    )]
    NotUnderstood {
        /// The file.
        path: String,
        /// What is wrong with it.
        problem: String,
    },
    /// The file could not be written.
    #[error(transparent)]
    Write(#[from] AtomicWriteError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    kind: String,
    name: String,
    hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    key: ProjectKey,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    root: String,
    trusted_at: Timestamp,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pinned: Vec<Pin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default, rename = "project", skip_serializing_if = "Vec::is_empty")]
    projects: Vec<Project>,
}

/// What the user has trusted of one project, read once: a value to ask
/// about each definition as it is met.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectTrust {
    project: Option<Project>,
}

impl ProjectTrust {
    /// A project nobody has trusted.
    #[must_use]
    pub fn untrusted() -> Self {
        Self::default()
    }

    /// Whether the user has trusted the project at all.
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        self.project.is_some()
    }

    /// How a definition stands.
    #[must_use]
    pub fn standing(&self, definition: &Definition) -> Standing {
        let Some(project) = &self.project else {
            return Standing::Untrusted;
        };
        let pinned = project
            .pinned
            .iter()
            .find(|pin| pin.kind == definition.kind.as_str() && pin.name == definition.name);
        match pinned {
            Some(pin) if pin.hash == definition.hash => Standing::Approved,
            Some(_) => Standing::Changed,
            None => Standing::New,
        }
    }

    /// Whether the project's rules, as its files hold them now, are the
    /// ones the user approved: `rules` is **every** rule of the project,
    /// whatever it does ([`Definition::of_rules`]). Any change, a rule that
    /// denies or asks added or dropped included, is a change to approve.
    ///
    /// A trusted project that has no rules, and had none pinned, needs no
    /// approval for them. One whose pinned rules are all gone has changed.
    #[must_use]
    pub fn rules_approved(&self, rules: &[Rule]) -> bool {
        match self.standing(&Definition::of_rules(rules)) {
            Standing::Approved => true,
            Standing::New => rules.is_empty(),
            Standing::Changed | Standing::Untrusted => false,
        }
    }
}

/// The trust store: which projects the user has trusted, and the hashes of
/// what they trusted.
#[derive(Debug, Clone)]
pub struct TrustStore {
    file: PathBuf,
    clock: SharedClock,
}

impl TrustStore {
    /// The store at `file`: `StateDir::trust_file` of `cortex-config`.
    #[must_use]
    pub fn new(file: impl Into<PathBuf>, clock: SharedClock) -> Self {
        Self {
            file: file.into(),
            clock,
        }
    }

    /// The file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.file
    }

    /// What the user has trusted of the project `key`.
    ///
    /// # Errors
    ///
    /// When the file cannot be read or is not understood in full. The
    /// caller treats the project as not trusted, and shows the error.
    pub fn project(&self, key: &ProjectKey) -> Result<ProjectTrust, TrustError> {
        let file = self.read()?;
        Ok(ProjectTrust {
            project: file
                .projects
                .into_iter()
                .find(|project| project.key == *key),
        })
    }

    /// Trusts a project for exactly these definitions, replacing whatever
    /// was pinned for it before. `root` is the workspace root as it should
    /// be shown to a person (`~/work/app`).
    ///
    /// # Errors
    ///
    /// When the file cannot be read, is not understood (it is then not
    /// overwritten), or cannot be written.
    pub fn trust(
        &self,
        key: &ProjectKey,
        root: &str,
        definitions: &[Definition],
    ) -> Result<ProjectTrust, TrustError> {
        self.update(|file| {
            file.projects.retain(|project| project.key != *key);
            let project = Project {
                key: key.clone(),
                root: root.to_owned(),
                trusted_at: self.clock.now().to_timestamp(),
                pinned: pins_of(definitions),
            };
            file.projects.push(project.clone());
            Ok((
                ProjectTrust {
                    project: Some(project),
                },
                true,
            ))
        })
    }

    /// Approves definitions of a project that is already trusted: new ones,
    /// and changed ones as they are now. Everything else that was pinned
    /// stays.
    ///
    /// # Errors
    ///
    /// When the file cannot be read, is not understood, or cannot be
    /// written; or when the project is not trusted, which approving one
    /// definition must not make it.
    pub fn approve(
        &self,
        key: &ProjectKey,
        definitions: &[Definition],
    ) -> Result<ProjectTrust, TrustError> {
        self.update(|file| {
            let Some(project) = file.projects.iter_mut().find(|project| project.key == *key) else {
                return Err(self.not_understood(
                    "the project is not trusted, so no definition of it can be approved".to_owned(),
                ));
            };
            for pin in pins_of(definitions) {
                project
                    .pinned
                    .retain(|old| !(old.kind == pin.kind && old.name == pin.name));
                project.pinned.push(pin);
            }
            Ok((
                ProjectTrust {
                    project: Some(project.clone()),
                },
                true,
            ))
        })
    }

    /// Takes trust away from a project. Says whether it was trusted.
    ///
    /// # Errors
    ///
    /// When the file cannot be read, is not understood, or cannot be
    /// written.
    pub fn revoke(&self, key: &ProjectKey) -> Result<bool, TrustError> {
        self.update(|file| {
            let before = file.projects.len();
            file.projects.retain(|project| project.key != *key);
            let was_trusted = file.projects.len() != before;
            Ok((was_trusted, was_trusted))
        })
    }

    /// The projects the user has trusted, as their key and the root they
    /// were trusted at, for `cortex permissions`.
    ///
    /// # Errors
    ///
    /// When the file cannot be read or is not understood.
    pub fn list(&self) -> Result<Vec<(ProjectKey, String)>, TrustError> {
        Ok(self
            .read()?
            .projects
            .into_iter()
            .map(|project| (project.key, project.root))
            .collect())
    }

    /// Reads the file, changes it and writes it if `change` says it changed
    /// (the second of what it returns), with the lock held throughout. Every
    /// change to the file goes through here.
    fn update<T>(
        &self,
        change: impl FnOnce(&mut File) -> Result<(T, bool), TrustError>,
    ) -> Result<T, TrustError> {
        let _lock = store_lock::hold(&self.file).map_err(|source| AtomicWriteError::Io {
            path: self.file.display().to_string(),
            step: "lock",
            source,
        })?;
        let mut file = self.read()?;
        let (answer, changed) = change(&mut file)?;
        if changed {
            self.write(&file)?;
        }
        Ok(answer)
    }

    fn not_understood(&self, problem: String) -> TrustError {
        TrustError::NotUnderstood {
            path: self.file.display().to_string(),
            problem,
        }
    }

    fn read(&self) -> Result<File, TrustError> {
        let read_error = |source| TrustError::Read {
            path: self.file.display().to_string(),
            source,
        };
        let size = match std::fs::metadata(&self.file) {
            Ok(found) => found.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(File {
                    version: VERSION,
                    projects: Vec::new(),
                });
            }
            Err(error) => return Err(read_error(error)),
        };
        if size > MAX_BYTES {
            return Err(self.not_understood(format!(
                "it is {size} bytes, more than a trust store could be"
            )));
        }
        let text = std::fs::read_to_string(&self.file).map_err(read_error)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let file: File = toml::from_str(text)
            .map_err(|error| self.not_understood(error.message().to_owned()))?;
        if file.version != VERSION {
            return Err(self.not_understood(format!(
                "its version is {}, and this version of Cortex-Harness reads version {VERSION}",
                file.version
            )));
        }
        Ok(file)
    }

    fn write(&self, file: &File) -> Result<(), TrustError> {
        // The directory was made when the lock was taken.
        let text = toml::to_string(file).map_err(|error| {
            self.not_understood(format!("it could not be written out: {error}"))
        })?;
        let options = AtomicWriteOptions {
            owner_only: true,
            ..AtomicWriteOptions::default()
        };
        write_atomic(&self.file, text.as_bytes(), &options, &*self.clock)?;
        Ok(())
    }
}

fn pins_of(definitions: &[Definition]) -> Vec<Pin> {
    let mut pins: Vec<Pin> = Vec::with_capacity(definitions.len());
    for definition in definitions {
        let pin = Pin {
            kind: definition.kind.as_str().to_owned(),
            name: definition.name.clone(),
            hash: definition.hash,
        };
        pins.retain(|old| !(old.kind == pin.kind && old.name == pin.name));
        pins.push(pin);
    }
    pins
}

#[cfg(test)]
mod tests {
    use cortex_protocol::lease::Effect;

    use super::*;

    fn hook(name: &str, parts: &[&str]) -> Definition {
        Definition::of(TrustResourceKind::Hook, name, parts.iter().copied())
    }

    fn trusted_with(definitions: &[Definition]) -> ProjectTrust {
        ProjectTrust {
            project: Some(Project {
                key: ProjectKey::new("k"),
                root: String::new(),
                trusted_at: Timestamp::new("2026-11-13T10:00:00Z"),
                pinned: pins_of(definitions),
            }),
        }
    }

    #[test]
    fn a_definition_is_its_parts_and_where_they_divide() {
        let base = hook("format", &["cargo", "fmt"]);
        assert_eq!(base, hook("format", &["cargo", "fmt"]));
        assert_ne!(base.hash, hook("format", &["cargo", "fmt", "--all"]).hash);
        assert_ne!(base.hash, hook("format", &["cargo fmt"]).hash);
        assert_ne!(base.hash, hook("format", &["cargofmt"]).hash);
        assert_ne!(base.hash, hook("format", &["cargo", "fm", "t"]).hash);
        assert_ne!(
            hook("x", &["rm", "-rf x"]).hash,
            hook("x", &["rm -rf", "x"]).hash
        );
        assert_ne!(hook("x", &[]).hash, hook("x", &[""]).hash);
    }

    #[test]
    fn a_definition_stands_by_its_hash() {
        let approved = hook("format", &["cargo", "fmt"]);
        let trust = trusted_with(std::slice::from_ref(&approved));
        assert!(trust.is_trusted());
        assert_eq!(trust.standing(&approved), Standing::Approved);
        // Changed since it was approved: disabled.
        let changed = hook("format", &["cargo", "fmt", "&&", "curl", "evil"]);
        assert_eq!(trust.standing(&changed), Standing::Changed);
        assert!(!trust.standing(&changed).is_approved());
        // Not among what the user saw.
        assert_eq!(
            trust.standing(&hook("deploy", &["./deploy"])),
            Standing::New
        );
        // The same name of another kind is another thing.
        let server = Definition::of(TrustResourceKind::McpServer, "format", ["cargo", "fmt"]);
        assert_eq!(trust.standing(&server), Standing::New);
        // And without trust nothing stands.
        let nobody = ProjectTrust::untrusted();
        assert!(!nobody.is_trusted());
        assert_eq!(nobody.standing(&approved), Standing::Untrusted);
    }

    #[test]
    fn a_projects_rules_are_pinned_together_whatever_their_order() {
        use cortex_protocol::lease::RuleAction;
        let rule = |effect: Effect, pattern: &str, action| Rule {
            effect,
            pattern: pattern.to_owned(),
            action,
        };
        let test = rule(Effect::Exec, "cargo test *", RuleAction::Allow);
        let build = rule(Effect::Exec, "cargo build *", RuleAction::Allow);
        let deny = rule(Effect::Write, "**/.env*", RuleAction::Deny);
        let seen = [test.clone(), build.clone(), deny.clone()];
        let trust = trusted_with(&[Definition::of_rules(&seen)]);

        assert!(trust.rules_approved(&seen));
        // Order and repeats do not matter.
        assert!(trust.rules_approved(&[deny.clone(), build.clone(), test.clone(), test.clone()]));
        // An added allow, a dropped one or a changed one is a change.
        let more = [
            test.clone(),
            build.clone(),
            deny.clone(),
            rule(Effect::Exec, "rm *", RuleAction::Allow),
        ];
        assert!(!trust.rules_approved(&more));
        assert!(!trust.rules_approved(&[test.clone(), deny.clone()]));
        assert!(!trust.rules_approved(&[
            test.clone(),
            rule(Effect::Exec, "cargo build", RuleAction::Allow),
            deny.clone()
        ]));
        // The same pattern for another effect is another rule.
        assert!(!trust.rules_approved(&[
            test.clone(),
            rule(Effect::Write, "cargo build *", RuleAction::Allow),
            deny.clone()
        ]));
        // So is the same pattern with another action.
        assert!(!trust.rules_approved(&[
            test.clone(),
            build.clone(),
            rule(Effect::Write, "**/.env*", RuleAction::Ask)
        ]));
        // A rule that holds back, dropped or added, is a change too: the
        // user trusted the allow rules with what held them back. Found by
        // the review of step 12.
        assert!(!trust.rules_approved(&[test.clone(), build.clone()]));
        assert!(!trust.rules_approved(&[
            test.clone(),
            build.clone(),
            deny.clone(),
            rule(Effect::Exec, "make deploy", RuleAction::Deny)
        ]));
        // All of them gone is a change.
        assert!(!trust.rules_approved(&[]));
        // A trusted project that had no rules pinned, and has none, needs
        // no approval; any rule it gains, of any action, does.
        let plain = trusted_with(&[]);
        assert!(plain.rules_approved(&[]));
        assert!(!plain.rules_approved(std::slice::from_ref(&deny)));
        assert!(!plain.rules_approved(std::slice::from_ref(&test)));
        assert!(!ProjectTrust::untrusted().rules_approved(&seen));
        assert!(!ProjectTrust::untrusted().rules_approved(&[]));
    }

    #[test]
    fn every_change_to_the_store_is_made_under_its_lock() {
        use cortex_base::FakeClock;

        use crate::store_lock::tests::{Scratch, is_held};

        let scratch = Scratch::new();
        let file = scratch.0.join("trust.toml");
        let clock: SharedClock =
            std::sync::Arc::new(FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap());
        let store = TrustStore::new(&file, clock);
        let key = ProjectKey::new("k");
        // Every public change goes through `update`, and `update` holds the
        // lock from the read to the write.
        let held = store.update(|_| Ok((is_held(&file), false))).unwrap();
        assert!(held, "the lock is held while the file is changed");
        assert!(!is_held(&file), "and let go after");
        store.trust(&key, "~/x", &[]).unwrap();
        store.approve(&key, &[]).unwrap();
        assert!(store.revoke(&key).unwrap());
        assert!(!store.project(&key).unwrap().is_trusted());
        assert!(!is_held(&file));

        // A lock that cannot be taken stops every change, and no read: so
        // each change takes it. (A directory stands where the lock goes.)
        store.trust(&key, "~/x", &[]).unwrap();
        let lock = crate::store_lock::lock_path(&file);
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        let refused = |result: Result<(), TrustError>| {
            assert!(
                matches!(
                    &result,
                    Err(TrustError::Write(AtomicWriteError::Io { step: "lock", .. }))
                ),
                "{result:?}"
            );
        };
        refused(store.trust(&ProjectKey::new("other"), "~/y", &[]).map(drop));
        refused(store.approve(&key, &[]).map(drop));
        refused(store.revoke(&key).map(drop));
        assert!(store.project(&key).unwrap().is_trusted());
        assert_eq!(store.list().unwrap().len(), 1);
    }
}
