//! The public data model of Cortex-Harness: the one contract that the TUI,
//! headless mode, the RPC server and the Coordinator speak
//! (`docs/architecture/11-rpc-protocol.md`).
//!
//! In-process front ends pass these types over channels as values. `cortex-rpc`
//! carries the same types as JSON-RPC 2.0, one JSON object per line. This
//! crate is types and their JSON form, and nothing else: no I/O and no
//! behaviour.
//!
//! # How it can grow without breaking
//!
//! The versioning rules of the protocol are built into the types:
//!
//! - **Unknown fields are ignored.** No type rejects a field it does not know.
//! - **Every enumeration is open.** A string enumeration keeps a value it
//!   does not know as `Unknown(String)`. A tagged union (an event, an item,
//!   a part, a notice) keeps an object whose tag it does not know as
//!   `Unknown`, whole. Both are written back as they arrived.
//! - **An optional field that is empty is left out,** and `null` is read as
//!   absent.
//! - **A breaking change needs a new [`PROTOCOL_VERSION`].** Anything else
//!   is an addition, announced by a capability in `initialize`.
//!
//! A value being *representable* is not the same as it being *accepted*: a
//! server that receives a level or a role it does not know refuses the
//! request. The types keep the value so that the refusal can name it.
//!
//! # Conventions on the wire
//!
//! - Field names and enumeration values are `snake_case`. The one exception
//!   is the level `read-only`, which is spelled as the command line spells it.
//! - Events are tagged by `type`, with dotted names (`item.delta`). Every
//!   other union is tagged by `kind`.
//! - Times are RFC 3339 strings in UTC. Durations are whole milliseconds in
//!   fields that end in `_ms`.
//! - Money is whole nano-dollars in fields named `usd_nanos`. Prices are
//!   nano-dollars per million tokens.
//!
//! The JSON Schema and the TypeScript declarations in `docs/protocol/` are
//! generated from these types by `cargo xtask schema`.

mod open;

pub mod error;
pub mod event;
pub mod ids;
pub mod item;
pub mod jsonrpc;
pub mod lease;
pub mod message;
pub mod methods;
pub mod model;
pub mod project;
pub mod session;
pub mod speed;
pub mod usage;

#[cfg(test)]
mod tests;

pub use error::{ErrorData, ErrorKind};
pub use event::{Event, Notice, SessionEvent};
pub use ids::{
    ApprovalId, ArtifactId, CallId, EntryId, ItemId, LeaseId, ModelId, PlanItemId, ProjectKey,
    ProviderId, RawJson, SessionId, Timestamp, TurnId,
};
pub use item::{Item, Plan, StopReason};
pub use lease::{Lease, Level};
pub use message::{Message, Part, Role};
pub use methods::{
    Op, OpResult, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, ServerNotification, ServerRequest,
    ServerResponse,
};
pub use model::{ModelInfo, ModelRole};
pub use open::UnknownVariant;
pub use speed::Speed;
pub use usage::{RequestUsage, SessionUsage, TokenUsage, UsageReport};

#[cfg(feature = "schema")]
pub mod schema;
