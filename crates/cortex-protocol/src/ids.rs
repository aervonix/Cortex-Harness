//! Identifiers and other small values that are strings on the wire.
//!
//! Each kind of identifier has its own type, so a turn id cannot be passed
//! where an item id is wanted. To a client they are opaque: compare them,
//! store them, send them back, and read nothing into their text.

use serde::{Deserialize, Serialize};

/// Defines an identifier: a string on the wire, a distinct type in Rust.
macro_rules! string_id {
    ($(#[doc = $doc:literal])* $name:ident) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps a string.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The value as written on the wire.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The value as an owned string.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_id! {
    /// A stored session.
    SessionId
}
string_id! {
    /// One turn of a session: from a prompt until the session is settled again.
    TurnId
}
string_id! {
    /// One item of a session: a message, a tool result, a notice or a summary.
    ItemId
}
string_id! {
    /// One tool call. The provider chose it, so it is unique only within its session.
    CallId
}
string_id! {
    /// A lease: what a session is allowed to do.
    LeaseId
}
string_id! {
    /// A stored artifact, such as the full output of a command or an attached image.
    ArtifactId
}
string_id! {
    /// One item of a plan.
    PlanItemId
}
string_id! {
    /// One entry of the project profile or memory.
    EntryId
}
string_id! {
    /// One stored approval.
    ApprovalId
}
string_id! {
    /// A model, as the catalog names it. Unique within its provider.
    ModelId
}
string_id! {
    /// A provider: an endpoint that speaks a protocol, by its configured name.
    ProviderId
}
string_id! {
    /// The key that names a project in the state directory. Never a path.
    ProjectKey
}
string_id! {
    /// A moment in time, in RFC 3339 form in UTC: `2026-11-12T10:15:02Z`.
    /// Fractional seconds are allowed.
    Timestamp
}
string_id! {
    /// JSON text kept exactly as a provider sent it, carried as a string so
    /// that no parser on the way can reorder keys, change escapes or round
    /// numbers. Replay sends it back unchanged. Clients treat it as opaque.
    RawJson
}
