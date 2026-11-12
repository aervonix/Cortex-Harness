//! A session's settings and state, as a client sets and sees them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{LeaseId, ModelId, ProviderId, SessionId, Timestamp, TurnId};
use crate::lease::Level;
use crate::model::ModelRole;
use crate::open::open_enum;
use crate::speed::Speed;

open_enum! {
    /// Whether the OS sandbox confines child processes on this machine.
    pub enum SandboxStatus {
        /// It does. Levels above `supervised` are available.
        Enforcing => "enforcing",
        /// It does not: the OS has no backend yet, or the kernel lacks what
        /// the backend needs. Commands that run are not confined.
        Off => "off",
    }
}

/// A change to the settings of one model role. A field that is absent is
/// left as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RoleSettings {
    /// The provider that serves the model. Needed only when two providers
    /// have a model with the same id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
    /// The model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelId>,
    /// The reasoning level: one of the model's own levels, or a portable
    /// alias (`minimal`, `low`, `medium`, `high`, `max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The speed for this role. For `main` it overrides the session's
    /// speed. The utility roles (`fast`, `judge`) do not follow the session's
    /// speed: they run at `standard` unless this is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
}

/// A change to a session's settings: the parameters of `session/set`, and
/// the settings a new session starts with. A field that is absent is left as
/// it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionSettings {
    /// The level (permission mode). Raising it needs the `user` role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    /// The session's speed: what the `main` role runs at unless it sets
    /// its own. The utility roles do not inherit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
    /// Settings per model role.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<ModelRole, RoleSettings>,
}

/// What one model role of a session is set to, and what is in effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RoleState {
    /// The provider.
    pub provider: ProviderId,
    /// The model.
    pub model: ModelId,
    /// The reasoning level as it was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The model's own level that the setting maps to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_in_effect: Option<String>,
    /// The speed this role is set to: its own setting; without one, the
    /// session's speed for `main` and `standard` for a utility role.
    /// Required, so that a reader never has to guess a default.
    pub speed: Speed,
    /// The speed its next request will ask for. It differs from `speed`
    /// when the model does not offer that speed, or while the speed is in a
    /// cooldown. Required.
    pub speed_in_effect: Speed,
    /// When a cooldown of `speed` ends, while one is running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_cooldown_until: Option<Timestamp>,
}

/// A session's metadata and current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionState {
    /// The session.
    pub session_id: SessionId,
    /// The workspace root. An absolute path on the server's machine.
    pub workspace: String,
    /// The session's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When it was created.
    pub created_at: Timestamp,
    /// The session it was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// The level of the lease in force.
    pub level: Level,
    /// The lease in force. `lease/get` returns its terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<LeaseId>,
    /// Whether the sandbox confines the session's commands.
    pub sandbox: SandboxStatus,
    /// The session's speed setting: what the `main` role runs at unless it
    /// sets its own. Required.
    pub speed: Speed,
    /// The model roles.
    pub roles: BTreeMap<ModelRole, RoleState>,
    /// The `seq` of the last event sent for this session. Events after it
    /// are live.
    pub seq: u64,
    /// The turn that is running, if one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_turn: Option<TurnId>,
    /// How many prompts are queued behind the running turn.
    #[serde(default)]
    pub queue_length: u32,
}

/// A stored session, in a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionSummary {
    /// The session.
    pub session_id: SessionId,
    /// The workspace root.
    pub workspace: String,
    /// The session's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it was last added to.
    pub updated_at: Timestamp,
    /// The session it was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// The main model it last used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelId>,
    /// How many turns it has.
    #[serde(default)]
    pub turns: u64,
}
