//! Project profile and memory entries, and a user's edits to the plan
//! (`docs/architecture/14-adaptation.md`, "Project knowledge", and
//! `docs/architecture/05-tools.md`, "`plan`").

use serde::{Deserialize, Serialize};

use crate::ids::{EntryId, ItemId, PlanItemId, SessionId, Timestamp};
use crate::open::{open_enum, open_union};

open_enum! {
    /// What an entry is.
    pub enum EntryKind {
        /// A named command: build, test, check, format, run.
        Command => "command",
        /// A rule in a sentence or two.
        Convention => "convention",
        /// A test that fails intermittently.
        Flaky => "flaky",
        /// Anything else worth keeping.
        Fact => "fact",
    }
}

open_enum! {
    /// Who an entry came from.
    pub enum EntrySourceKind {
        /// The user wrote it.
        User => "user",
        /// The harness learned it in a session.
        Learned => "learned",
    }
}

/// Where an entry came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntrySource {
    /// Who it came from.
    pub kind: EntrySourceKind,
    /// For `learned`: the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// For `learned`: the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    /// For `learned`: when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
}

open_enum! {
    /// How the harness came to hold an entry.
    pub enum EntryBasis {
        /// The harness saw it happen.
        Observed => "observed",
        /// A model inferred it.
        Proposed => "proposed",
    }
}

open_enum! {
    /// Whether an entry is in use.
    pub enum EntryStatus {
        /// Waiting for the user to accept it. Never in the prompt.
        Proposed => "proposed",
        /// In use.
        Active => "active",
        /// Kept, and not in use.
        Disabled => "disabled",
    }
}

open_enum! {
    /// Which file an entry lives in.
    pub enum EntryScope {
        /// The project's shared profile, in the repository.
        Shared => "shared",
        /// This user's private store for the project.
        Private => "private",
    }
}

open_enum! {
    /// What a command entry is for.
    pub enum CommandPurpose {
        /// Builds the project.
        Build => "build",
        /// Runs its tests.
        Test => "test",
        /// Runs its checks: lints, types.
        Check => "check",
        /// Formats it.
        Format => "format",
        /// Runs it.
        Run => "run",
    }
}

/// The command of a `command` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntryCommand {
    /// What it is for.
    pub purpose: CommandPurpose,
    /// The command line.
    pub command: String,
    /// The directory it runs in, relative to the workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// How long it usually takes, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usual_duration_ms: Option<u64>,
    /// Whether it only reads.
    #[serde(default)]
    pub read_only: bool,
}

open_enum! {
    /// The form of a check.
    pub enum EntryCheckKind {
        /// The path in `path` exists.
        Exists => "exists",
        /// A search in `path` finds `pattern`.
        Contains => "contains",
        /// The command in `command` exits with success.
        Command => "command",
    }
}

/// A cheap, read-only test that shows an entry still holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntryCheck {
    /// The form of the check.
    pub kind: EntryCheckKind,
    /// For `exists` and `contains`: the path, relative to the workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// For `contains`: the pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// For `command`: the command line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// One entry of the project profile or memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectEntry {
    /// The entry's id.
    pub id: EntryId,
    /// What it is.
    pub kind: EntryKind,
    /// The entry in words: the rule, the fact, the test's name, or what the
    /// command is.
    pub text: String,
    /// For a `command` entry: the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<EntryCommand>,
    /// Where it was learned: a path and line, a command, or the user's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Where it came from.
    pub source: EntrySource,
    /// How the harness came to hold it. Absent for an entry the user wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<EntryBasis>,
    /// Whether it is in use.
    pub status: EntryStatus,
    /// Which file it lives in.
    pub scope: EntryScope,
    /// When it was last seen to hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<Timestamp>,
    /// When it stops being used unless confirmed again. Entries the user
    /// wrote do not expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    /// A test that shows it still holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<EntryCheck>,
}

open_enum! {
    /// What a user does to an entry.
    pub enum EntryAction {
        /// Makes a proposed or disabled entry active.
        Accept => "accept",
        /// Replaces its text.
        Edit => "edit",
        /// Takes it out of use and keeps it.
        Disable => "disable",
        /// Removes it.
        Delete => "delete",
        /// Moves it to the shared profile.
        Promote => "promote",
    }
}

/// One change to a project entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntryChange {
    /// The entry.
    pub id: EntryId,
    /// What to do to it.
    pub action: EntryAction,
    /// For `edit`: the new text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Puts the plan's items in a new order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanReorder {
    /// Every item's id, in the new order.
    pub order: Vec<PlanItemId>,
}

/// Names one plan item: the payload of `skip` and `remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanItemRef {
    /// The item.
    pub id: PlanItemId,
}

/// Replaces a plan item's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanReword {
    /// The item.
    pub id: PlanItemId,
    /// The new text.
    pub text: String,
}

/// Adds a plan item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanAdd {
    /// The task.
    pub text: String,
    /// The item it goes after. Absent: at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<PlanItemId>,
}

open_union! {
    /// One edit a user makes to the plan. It takes effect at once, and
    /// reaches the model as a notice at the next safe boundary.
    pub enum PlanEdit [kind] {
        /// Put the items in a new order.
        Reorder(PlanReorder) => "reorder",
        /// Mark an item as skipped.
        Skip(PlanItemRef) => "skip",
        /// Replace an item's text.
        Reword(PlanReword) => "reword",
        /// Add an item.
        Add(PlanAdd) => "add",
        /// Remove an item.
        Remove(PlanItemRef) => "remove",
    }
}
