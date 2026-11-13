//! The session store (`docs/architecture/07-sessions-and-context.md`).
//!
//! A session is an append-only JSONL log in a directory of its own, with
//! its artifacts and the file contents kept for undo beside it, grouped by
//! project, with an index for listing.
//!
//! | Module | What it gives |
//! |---|---|
//! | [`layout`] | [`SessionsRoot`], [`ProjectSessions`] and [`SessionDir`]: where the files are. |
//! | [`entry`] | [`Entry`] and [`Body`]: the kinds of entry and their form on disk. |
//! | [`log`] | [`Log`]: a log read as a tree, with the active branch, entries read from the file when asked for. |
//! | [`session`] | [`Session`]: a log open for writing, with resume and repair, rewind, undo, switch and fork. |
//! | [`file`] | [`Durability`]: when the log is flushed. |
//! | [`store`] | [`DiskStore`]: artifacts and stored file contents. |
//! | [`snapshot`] | [`RoundCheckpoint`], and the plans and results of a rewind. |
//! | [`base`] | The workspace's starting state, taken in the background. |
//! | [`index`] | [`SessionIndex`]: listing a project's sessions. |
//! | [`redact`] | [`Redactor`]: the hook that masks secrets before anything is written. |
//!
//! # What it promises
//!
//! - **Nothing half-read.** A last line a crash cut off is ignored and
//!   reported; a damaged line elsewhere is skipped and reported; an entry
//!   of a kind this build does not know is skipped and kept.
//! - **Memory and disk agree.** An entry is in a session only once it is on
//!   disk. A failed write leaves both as they were.
//! - **A lease is never read in part.** Lease records go through the
//!   protocol's strict decoder, and one this build does not understand in
//!   full, or that a newer format wrote, is refused whole
//!   (`docs/decisions/0028-enforced-values-fail-closed.md`).
//! - **Undo never guesses.** A file whose earlier state is not known is
//!   reported as one that cannot be put back, and is never removed or
//!   overwritten.
//! - **Private files.** Everything it creates can be opened by the current
//!   user alone, on Linux and on Windows ([`cortex_base::private`]). On a
//!   file system that keeps no permissions it still writes, and says so.
//!
//! # Blocking
//!
//! Every function blocks. Async callers run them on the blocking pool.
//! Paths, the clock and the settings are inputs: this crate depends on
//! `cortex-protocol` and `cortex-base` only.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the session store blocks throughout; async callers run it on the blocking pool"
)]

pub mod base;
pub mod entry;
pub mod error;
pub mod file;
pub mod index;
pub mod layout;
mod lock;
pub mod log;
pub mod redact;
mod scan;
pub mod session;
pub mod snapshot;
pub mod store;

pub use base::{BaseCapture, BaseLimits, BaseState, Git, KeepGate, SystemGit};
pub use entry::{Before, Body, Entry, EntryId, FORMAT_VERSION, Kind};
pub use error::{Result, SessionError};
pub use file::Durability;
pub use index::{IndexRecord, SessionIndex};
pub use layout::{ProjectSessions, SessionDir, SessionsRoot};
pub use log::{Log, OpenReport};
pub use redact::{NoRedaction, Redactor};
pub use scan::CutTail;
pub use session::{Session, SessionOptions};
pub use snapshot::{
    FileRestore, FileStatus, Repaired, Restore, RewindDone, RewindPlan, RoundCheckpoint, SwitchDone,
};
pub use store::{ArtifactFile, DiskStore, KEEP_LIMIT};
