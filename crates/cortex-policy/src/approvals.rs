//! Approvals at three scopes
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Approvals").
//!
//! When the engine's answer is "ask", a person chooses:
//!
//! | Choice | What it adds |
//! |---|---|
//! | Allow once | Nothing. This call only. |
//! | Allow for this session | Rules held in memory, gone when the session ends ([`SessionApprovals`]). |
//! | Always allow in this project | Rules in the user's per-project state, not in the repository ([`ApprovalStore`]). |
//! | Deny | Nothing. |
//!
//! **An approval never outlives its scope**, and never reaches further than
//! what was asked. What a choice would add is worked out before the
//! question is put ([`crate::engine::Question::lifts`]), so a front end
//! shows it in full: "always allow `cargo build *` in this project". Where
//! no rule could express the approval safely (a command the analysis does
//! not model, a question an `ask` rule raised), only "allow once" is on
//! offer.
//!
//! # The stored file
//!
//! `approvals/<project key>.toml` in the state directory:
//!
//! ```toml
//! version = 1
//!
//! [[approval]]
//! id = "ap_1"
//! created_at = "2026-11-13T10:15:02Z"
//! rule = { effect = "exec", match = "cargo build *", action = "allow" }
//! ```
//!
//! - **It is written owner-only, on every write**, and atomically
//!   (`cortex_base::write_atomic` with `owner_only`). Its directory is
//!   created private if it is missing.
//! - **It is read strictly.** A rule goes through the protocol's own strict
//!   reader; a key this build does not know, a newer `version`, or a rule
//!   that does anything but allow, refuses the whole file
//!   (`docs/decisions/0028-enforced-values-fail-closed.md`). A file that is
//!   refused grants nothing: stored approvals only ever allow, so reading
//!   none of them is the safe side.
//! - **It belongs to the user.** It is keyed by the project and kept
//!   outside the repository, so trusting the project is not needed for it
//!   to apply, and nothing in a repository can write it: the state
//!   directory is closed to every tool
//!   ([`Protected`](crate::rules::Protected)).

#![allow(
    clippy::disallowed_methods,
    reason = "blocking file calls; the store is used at start-up and on the blocking pool"
)]

use std::io;
use std::path::{Path, PathBuf};

use cortex_base::{AtomicWriteError, AtomicWriteOptions, SharedClock, private, write_atomic};
use cortex_protocol::ids::{ApprovalId, Timestamp};
use cortex_protocol::lease::{Rule, RuleAction};
use cortex_protocol::methods::{ApprovalScope, StoredApproval};
use serde::{Deserialize, Serialize};

/// The version of the file's format this build writes and reads.
const VERSION: u32 = 1;

/// The largest file read. A file of approvals is a few kilobytes.
const MAX_BYTES: u64 = 1024 * 1024;

/// Why stored approvals could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    /// The file could not be read.
    #[error("the stored approvals in `{path}` could not be read: {source}")]
    Read {
        /// The file.
        path: String,
        /// Why.
        source: io::Error,
    },
    /// The file is not one this build understands in full. It grants
    /// nothing, and is left as it is.
    #[error(
        "the stored approvals in `{path}` are not understood, so none of them applies: {problem}. \
         The file was left as it is"
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

/// One entry of the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: ApprovalId,
    created_at: Timestamp,
    rule: Rule,
}

/// The file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default, rename = "approval", skip_serializing_if = "Vec::is_empty")]
    approvals: Vec<Entry>,
}

/// The "always allow in this project" rules of one project, in the user's
/// state directory.
#[derive(Debug, Clone)]
pub struct ApprovalStore {
    file: PathBuf,
    clock: SharedClock,
}

impl ApprovalStore {
    /// The store at `file`: `StateDir::approvals_file` of `cortex-config`
    /// for the project's key.
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

    /// The stored approvals. A file that is not there holds none.
    ///
    /// # Errors
    ///
    /// When the file cannot be read, or is not understood in full. In both
    /// cases no stored approval applies.
    pub fn load(&self) -> Result<Vec<StoredApproval>, ApprovalError> {
        Ok(self
            .read()?
            .approvals
            .into_iter()
            .map(|entry| StoredApproval {
                id: entry.id,
                rule: entry.rule,
                scope: ApprovalScope::Project,
                created_at: entry.created_at,
            })
            .collect())
    }

    /// Adds allow rules, and returns them as stored. A rule that is already
    /// stored is not stored twice: the entry it has is returned.
    ///
    /// # Errors
    ///
    /// When the file cannot be read, is not understood (it is then not
    /// overwritten), or cannot be written; or when a rule does anything but
    /// allow.
    pub fn add(&self, rules: &[Rule]) -> Result<Vec<StoredApproval>, ApprovalError> {
        if let Some(rule) = rules.iter().find(|rule| rule.action != RuleAction::Allow) {
            return Err(self.not_understood(format!(
                "an approval can only allow, and `{} {} {}` does not",
                rule.action.as_str(),
                rule.effect.as_str(),
                rule.pattern
            )));
        }
        let mut file = self.read()?;
        let now = self.clock.now().to_timestamp();
        let mut stored = Vec::with_capacity(rules.len());
        for rule in rules {
            let entry = match file.approvals.iter().find(|entry| entry.rule == *rule) {
                Some(entry) => entry.clone(),
                None => {
                    let entry = Entry {
                        id: next_id(&file.approvals),
                        created_at: now.clone(),
                        rule: rule.clone(),
                    };
                    file.approvals.push(entry.clone());
                    entry
                }
            };
            stored.push(StoredApproval {
                id: entry.id,
                rule: entry.rule,
                scope: ApprovalScope::Project,
                created_at: entry.created_at,
            });
        }
        self.write(&file)?;
        Ok(stored)
    }

    /// Removes approvals by id, and says how many were there.
    ///
    /// # Errors
    ///
    /// When the file cannot be read, is not understood, or cannot be
    /// written.
    pub fn remove(&self, ids: &[ApprovalId]) -> Result<usize, ApprovalError> {
        let mut file = self.read()?;
        let before = file.approvals.len();
        file.approvals.retain(|entry| !ids.contains(&entry.id));
        let removed = before - file.approvals.len();
        if removed > 0 {
            self.write(&file)?;
        }
        Ok(removed)
    }

    fn not_understood(&self, problem: String) -> ApprovalError {
        ApprovalError::NotUnderstood {
            path: self.file.display().to_string(),
            problem,
        }
    }

    fn read(&self) -> Result<File, ApprovalError> {
        let read_error = |source| ApprovalError::Read {
            path: self.file.display().to_string(),
            source,
        };
        let size = match std::fs::metadata(&self.file) {
            Ok(found) => found.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(File {
                    version: VERSION,
                    approvals: Vec::new(),
                });
            }
            Err(error) => return Err(read_error(error)),
        };
        if size > MAX_BYTES {
            return Err(self.not_understood(format!(
                "it is {size} bytes, more than a file of approvals could be"
            )));
        }
        let text = std::fs::read_to_string(&self.file).map_err(read_error)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let file: File =
            toml::from_str(text).map_err(|error| self.not_understood(error.message().to_owned()))?;
        if file.version != VERSION {
            return Err(self.not_understood(format!(
                "its version is {}, and this version of Cortex-Harness reads version {VERSION}",
                file.version
            )));
        }
        if let Some(entry) = file
            .approvals
            .iter()
            .find(|entry| entry.rule.action != RuleAction::Allow)
        {
            return Err(self.not_understood(format!(
                "the approval `{}` does not allow: a stored approval can do nothing else",
                entry.id
            )));
        }
        Ok(file)
    }

    fn write(&self, file: &File) -> Result<(), ApprovalError> {
        let text = toml::to_string(file)
            .map_err(|error| self.not_understood(format!("it could not be written out: {error}")))?;
        if let Some(directory) = self
            .file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            private::create_dir_all(directory).map_err(|source| AtomicWriteError::Io {
                path: self.file.display().to_string(),
                step: "create the directory",
                source,
            })?;
        }
        let options = AtomicWriteOptions {
            owner_only: true,
            ..AtomicWriteOptions::default()
        };
        write_atomic(&self.file, text.as_bytes(), &options, &*self.clock)?;
        Ok(())
    }
}

/// The next id: one more than the largest number in use. Ids are for
/// naming an approval to take it back, and mean nothing once it is gone.
fn next_id(entries: &[Entry]) -> ApprovalId {
    let largest = entries
        .iter()
        .filter_map(|entry| entry.id.as_str().strip_prefix("ap_")?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    ApprovalId::new(format!("ap_{}", largest + 1))
}

/// The "allow for this session" rules: held in memory, and gone with the
/// session.
#[derive(Debug, Clone, Default)]
pub struct SessionApprovals {
    approvals: Vec<StoredApproval>,
    made: u64,
}

impl SessionApprovals {
    /// No approvals.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds allow rules, as given at `now`. A rule already held is not held
    /// twice. Rules that do anything but allow are left out: an approval
    /// can only allow.
    pub fn add(&mut self, rules: &[Rule], now: &Timestamp) -> Vec<StoredApproval> {
        let mut added = Vec::new();
        for rule in rules.iter().filter(|rule| rule.action == RuleAction::Allow) {
            if let Some(held) = self.approvals.iter().find(|held| held.rule == *rule) {
                added.push(held.clone());
                continue;
            }
            self.made += 1;
            let approval = StoredApproval {
                id: ApprovalId::new(format!("ap_s{}", self.made)),
                rule: rule.clone(),
                scope: ApprovalScope::Session,
                created_at: now.clone(),
            };
            self.approvals.push(approval.clone());
            added.push(approval);
        }
        added
    }

    /// Removes approvals by id, and says how many were there.
    pub fn remove(&mut self, ids: &[ApprovalId]) -> usize {
        let before = self.approvals.len();
        self.approvals.retain(|approval| !ids.contains(&approval.id));
        before - self.approvals.len()
    }

    /// The approvals held.
    #[must_use]
    pub fn list(&self) -> &[StoredApproval] {
        &self.approvals
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::lease::Effect;

    use super::*;

    fn allow(pattern: &str) -> Rule {
        Rule {
            effect: Effect::Exec,
            pattern: pattern.to_owned(),
            action: RuleAction::Allow,
        }
    }

    #[test]
    fn session_approvals_are_held_once_and_removed_by_id() {
        let mut session = SessionApprovals::new();
        let now = Timestamp::new("2026-11-13T10:00:00Z");
        let first = session.add(&[allow("cargo build *"), allow("cargo test *")], &now);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].id, ApprovalId::new("ap_s1"));
        assert_eq!(first[0].scope, ApprovalScope::Session);
        // The same rule again is the approval already held.
        let again = session.add(&[allow("cargo build *")], &now);
        assert_eq!(again, [first[0].clone()]);
        assert_eq!(session.list().len(), 2);
        // An approval can only allow.
        let deny = Rule {
            action: RuleAction::Deny,
            ..allow("rm *")
        };
        assert!(session.add(&[deny], &now).is_empty());
        assert_eq!(session.list().len(), 2);

        assert_eq!(session.remove(&[ApprovalId::new("ap_s1"), ApprovalId::new("ap_x")]), 1);
        assert_eq!(session.list().len(), 1);
        // A removed approval's id is not given to the next one.
        let next = session.add(&[allow("make *")], &now);
        assert_eq!(next[0].id, ApprovalId::new("ap_s3"));
    }
}
