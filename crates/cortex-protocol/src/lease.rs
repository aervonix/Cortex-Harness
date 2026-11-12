//! The wire form of levels and leases: the one autonomy model
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Autonomy: one model").
//!
//! These types say what a lease *is* on the wire and in the session log. What
//! a lease *means* (evaluating rules, narrowing, refusing a level the sandbox
//! cannot back) is `cortex-policy`.
//!
//! # Enforced values fail closed
//!
//! Everywhere else in the protocol an unknown field is ignored and an
//! unknown value is kept. Here that would be a security hole: a build that
//! dropped a restriction it did not understand would turn a narrower lease
//! into a wider one. So the types in this module are the exception
//! (`docs/decisions/0028-enforced-values-fail-closed.md`):
//!
//! - **An unknown field is an error,** in a lease, a grant, a rule, a sandbox
//!   policy, a path set, a budget, the hard limits, a stop condition, an
//!   expiry and a grantor.
//! - **An unknown value is an error:** a level, an effect, a rule action, a
//!   network setting, a kind of stop condition, of expiry or of grantor.
//! - **The whole value is refused.** Nothing is ignored and nothing is kept.
//!   [`Lease::from_wire`] and its companions give the refusal as an error
//!   with the code `lease_not_understood`.
//!
//! [`Level`] and [`Effect`] are also used where they only inform (a
//! session's state, an approval prompt), so as types they are open. Inside a
//! grant or a rule a value that is not listed is refused all the same.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ErrorData, ErrorKind, code};
use crate::ids::{LeaseId, ModelId, Timestamp};
use crate::open::{closed_enum, closed_union, open_enum};

open_enum! {
    /// How much a session may do without asking: who decides when no rule
    /// has settled the question. The command line calls a session's level
    /// its permission mode.
    pub enum Level {
        /// Reads run. Edits are refused. Commands ask unless they only read.
        ReadOnly => "read-only",
        /// The default. Reads and edits inside the scope run, with a snapshot
        /// to undo them. Commands ask unless they are on a safe list.
        Supervised => "supervised",
        /// As `supervised`, and the kinds of command the lease names run
        /// without asking, inside an enforcing sandbox.
        Guided => "guided",
        /// Commands run without asking, inside an enforcing sandbox.
        Autonomous => "autonomous",
        /// Everything runs without asking and without a sandbox.
        Unrestricted => "unrestricted",
    }
}

open_enum! {
    /// What a planned tool call does.
    pub enum Effect {
        /// Reads a path.
        Read => "read",
        /// Writes a path.
        Write => "write",
        /// Runs a command.
        Exec => "exec",
        /// Reaches a host.
        Network => "network",
        /// Calls an extension or MCP tool.
        Ext => "ext",
    }
}

closed_enum! {
    /// What a rule decides.
    pub enum RuleAction {
        /// The effect may happen without asking.
        Allow => "allow",
        /// The supervisor is asked.
        Ask => "ask",
        /// The effect is refused.
        Deny => "deny",
    }
}

/// A permission rule. `deny` beats `ask` beats `allow`, whatever the order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// The effect the rule is about. An effect that is not listed is refused.
    #[serde(deserialize_with = "crate::open::listed")]
    #[cfg_attr(feature = "schema", schemars(schema_with = "schema::effect"))]
    pub effect: Effect,
    /// What it matches: a path glob, a command pattern, a host, or a
    /// `server/tool` name.
    #[serde(rename = "match")]
    pub pattern: String,
    /// What it decides.
    pub action: RuleAction,
}

/// A set of paths: everything, or the trees under some roots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PathSet {
    /// Every path. When set, `roots` is ignored.
    #[serde(default)]
    pub all: bool,
    /// The roots of the trees in the set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
}

closed_enum! {
    /// Whether sandboxed processes may use the network. There is no per-host list.
    pub enum NetworkAccess {
        /// No network.
        Off => "off",
        /// The whole network.
        Full => "full",
    }
}

/// What a child process may touch, whatever it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SandboxPolicy {
    /// What may be read.
    pub read: PathSet,
    /// What may be written.
    pub write: PathSet,
    /// Paths inside `write` that must not be changed.
    #[serde(default)]
    pub protect: PathSet,
    /// Whether the network may be used.
    pub network: NetworkAccess,
}

/// What a lease may spend. A field that is absent is not limited by this lease.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Money, in nano-dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_nanos: Option<u64>,
    /// Tokens, input and output together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Requests to a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<u64>,
    /// Wall-clock time, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock_ms: Option<u64>,
}

/// What no level relaxes. Set by the user in the grant and enforced by the
/// harness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HardLimits {
    /// Whether work may be merged into the repository's main branch.
    #[serde(default)]
    pub merge_to_main: bool,
    /// The most sessions a team may run at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_size: Option<u32>,
}

/// What a lease allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// The level. A level that is not listed is refused.
    #[serde(deserialize_with = "crate::open::listed")]
    #[cfg_attr(feature = "schema", schemars(schema_with = "schema::level"))]
    pub level: Level,
    /// The rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    /// The sandbox policy.
    pub sandbox: SandboxPolicy,
    /// The models that may be used. Empty means the configured ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelId>,
    /// What may be spent.
    #[serde(default)]
    pub budget: Budget,
    /// What no level relaxes.
    #[serde(default)]
    pub limits: HardLimits,
}

/// Stop when the project's checks have failed this many rounds in a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct VerificationFailedStop {
    /// The number of failed rounds in a row.
    pub rounds: u32,
}

/// Stop when an action outside the lease's scope is needed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OutOfScopeStop {}

/// Stop when spending passes a threshold below the budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SpendingStop {
    /// The threshold, in nano-dollars.
    pub usd_nanos: u64,
}

/// Stop at a moment in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TimeStop {
    /// The moment.
    pub at: Timestamp,
}

closed_union! {
    /// A condition on which the holder of a lease must stop and report. The
    /// harness checks it. It is data, not an instruction to the model.
    pub enum StopCondition [kind] {
        /// The project's checks failed some rounds in a row.
        VerificationFailed(VerificationFailedStop) => "verification_failed",
        /// An action outside the scope was needed.
        OutOfScope(OutOfScopeStop) => "out_of_scope",
        /// Spending passed a threshold.
        Spending(SpendingStop) => "spending",
        /// The time ran out.
        Time(TimeStop) => "time",
    }
}

closed_enum! {
    /// When a lease ends.
    pub enum ExpiryKind {
        /// It does not end.
        Never => "never",
        /// At the moment in `at`.
        At => "at",
        /// When the task it was granted for is done.
        EndOfTask => "end_of_task",
        /// When the session ends.
        EndOfSession => "end_of_session",
    }
}

/// When a lease ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Expiry {
    /// Which kind of end.
    pub kind: ExpiryKind,
    /// For `at`: the moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
}

closed_enum! {
    /// Who granted a lease.
    pub enum GrantorKind {
        /// The user. Only a person grants, extends or revokes a lease.
        User => "user",
        /// The harness, narrowing the parent session's lease in `lease_id`.
        Parent => "parent",
    }
}

/// Who granted a lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Grantor {
    /// Which kind of grantor.
    pub kind: GrantorKind,
    /// For `parent`: the lease this one was narrowed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<LeaseId>,
}

/// Everything a session is allowed to do, as one value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Lease {
    /// The lease's id.
    pub id: LeaseId,
    /// What is allowed.
    pub grant: Grant,
    /// When the holder must stop and report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_when: Vec<StopCondition>,
    /// When the lease ends.
    pub expires: Expiry,
    /// Who granted it.
    pub granted_by: Grantor,
}

/// Reads an enforced value, and turns a failure into the refusal a client
/// is sent.
///
/// The error is boxed because it is large and rare: a lease that reads is
/// the case to keep cheap.
fn read_enforced<T: DeserializeOwned>(what: &str, value: Value) -> Result<T, Box<ErrorData>> {
    T::deserialize(value).map_err(|error| {
        Box::new(ErrorData::new(
            ErrorKind::Permission,
            code::LEASE_NOT_UNDERSTOOD,
            format!(
                "The {what} was refused: {error}. A {what} is enforced, so one that this \
                 version does not understand in full is refused whole, never applied in part."
            ),
        ))
    })
}

impl Lease {
    /// Reads a lease. A lease with a field or a value this version does not
    /// know is refused whole.
    ///
    /// # Errors
    ///
    /// With the code `lease_not_understood`, when the value is not a lease
    /// this version understands in full.
    pub fn from_wire(value: Value) -> Result<Self, Box<ErrorData>> {
        read_enforced("lease", value)
    }
}

impl Grant {
    /// Reads a grant. A grant with a field or a value this version does not
    /// know is refused whole.
    ///
    /// # Errors
    ///
    /// With the code `lease_not_understood`, when the value is not a grant
    /// this version understands in full.
    pub fn from_wire(value: Value) -> Result<Self, Box<ErrorData>> {
        read_enforced("grant", value)
    }
}

impl Rule {
    /// Reads a rule. A rule with a field or a value this version does not
    /// know is refused whole.
    ///
    /// # Errors
    ///
    /// With the code `lease_not_understood`, when the value is not a rule
    /// this version understands in full.
    pub fn from_wire(value: Value) -> Result<Self, Box<ErrorData>> {
        read_enforced("rule", value)
    }
}

/// Schemas for the two open enumerations where they sit in an enforced
/// value: the listed values and nothing else.
#[cfg(feature = "schema")]
mod schema {
    use schemars::{Schema, SchemaGenerator, json_schema};

    use super::{Effect, Level};

    pub(super) fn level(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "enum": Level::KNOWN })
    }

    pub(super) fn effect(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "enum": Effect::KNOWN })
    }
}
