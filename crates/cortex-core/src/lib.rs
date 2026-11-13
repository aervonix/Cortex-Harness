//! The core: every part of the harness wired into one thing that takes
//! operations and emits events (`docs/architecture/00-overview.md`).
//!
//! This is the only crate that knows all the parts. Each of them was built
//! against a seam of its own (a trait it defines, or a type it takes) and
//! none of them can see the others. The modules here are those seams,
//! filled:
//!
//! | Module | Joins |
//! |---|---|
//! | [`core`] | Front ends to sessions: operations, client roles, the requests a front end answers. |
//! | [`events`] | The loop's events to each client's bounded queue, with deltas merged. |
//! | `assemble` | Configuration, trust, the engine, the models, the store and the tools into one session. |
//! | `store` | The loop's session store to the session log. |
//! | `content` | The tools' content store and the provider layer's image source to the session's files. |
//! | `checkpoints` | The editing tools and the shell tool to the round's checkpoint. |
//! | `policy` | The loop's policy, the tools' read gate and the base state's keep gate to the permission engine. |
//! | `provider` | Configuration and credentials to model clients. |
//! | `redact` | The secret filter to the session log, and the rules for provider blocks. |
//!
//! A front end needs [`Core`], [`CoreOptions`], [`ClientInfo`], and the
//! protocol's own types: an [`Op`](cortex_protocol::methods::Op) in, an
//! [`Outgoing`] out.

mod assemble;
mod checkpoints;
mod clients;
mod content;
pub mod core;
pub mod events;
mod policy;
mod provider;
mod redact;
mod rules;
mod store;

pub use crate::clients::{ClientId, ClientInfo};
pub use crate::core::{Client, Core, CoreOptions, OpError};
pub use crate::events::{EventQueue, Outgoing, QUEUE_CAPACITY};
pub use crate::policy::PermissionDenial;
pub use crate::provider::{ANTHROPIC_KEY_ENV, Connector, EFFORT_ALIASES, KeyFound};
pub use crate::rules::parse_rule;
