//! The agent loop (`docs/architecture/03-agent-loop.md`).
//!
//! A [`Session`] is one conversation's loop: an explicit state machine that
//! takes a prompt, sends requests to a model, runs the tools the model
//! calls, and ends the turn when nothing automatic remains. It owns no I/O
//! of its own. Everything it touches it reaches through a trait, so every
//! behaviour here is tested with a scripted model and fake tools, with no
//! network and no real clock.
//!
//! | Module | What it gives |
//! |---|---|
//! | [`turn`] | [`Session`], the loop, and [`SessionParts`], what one is made of. |
//! | [`session`] | [`SessionHandle`]: prompts, steering, interruption, settings. |
//! | [`state`] | [`TurnState`], the table of transitions, and [`Observer`]. |
//! | [`store`] | [`SessionStore`], what the loop needs from a session's log, and [`MemorySessionStore`]. |
//! | [`policy`] | [`Policy`], what it asks of the permission engine, and [`AllowAll`]. |
//! | [`toolset`] | [`ToolSet`], the tools a session offers. |
//! | [`context`] | [`Context`], what is sent to the model, and [`StubContext`]. |
//! | [`verify`] | [`Verifier`], the verification stage, and [`NoVerifier`]. |
//! | [`events`] | [`EventSink`], where a session's events go. |
//! | [`stats`] | [`Stats`] and the statistics events. |
//! | [`ledger`] | [`Ledger`] and [`Account`]: limits that several sessions can share. |
//! | [`guard`] | [`RepetitionLimits`], for the repetition guard. |
//! | [`speed`] | [`SpeedSettings`]: the speed tier of the session and its roles. |
//! | [`config`] | [`Models`], [`Workspace`], [`SessionInfo`] and [`LoopConfig`]. |
//!
//! # Running a session
//!
//! [`Session::open`] reads the session from its store and returns the loop
//! and a [`SessionHandle`]. The loop is one future, [`Session::run`], for
//! one task. It needs a Tokio runtime around it: each tool call, each plan
//! and each question to the user is a task of its own, and blocking work
//! goes to the runtime's blocking pool.
//!
//! Any number of sessions run on one runtime. They share nothing but what
//! they are given to share: the model clients, the table of speed-tier
//! cooldowns, and any [`Account`] of the ledger.
//!
//! # What the loop promises
//!
//! - **`turn.completed` means settled:** no retry pending, no tool running,
//!   nothing more to come for that prompt.
//! - **Every tool call gets exactly one result,** in the model's call
//!   order, whatever happened to it.
//! - **After an interruption the history is valid** for the next request.
//! - **Nothing with a side effect runs for a response that did not end.**

mod assistant;
mod round;
mod usage;

pub mod config;
pub mod context;
pub mod events;
pub mod guard;
pub mod ledger;
pub mod policy;
pub mod session;
pub mod speed;
pub mod state;
pub mod stats;
pub mod store;
pub mod toolset;
pub mod turn;
pub mod verify;

pub use config::{LoopConfig, Models, RoleModel, SessionInfo, Workspace};
pub use context::{Assembled, Context, ContextError, StubContext};
pub use events::EventSink;
pub use guard::RepetitionLimits;
pub use ledger::{Account, Ledger, LimitReached, Spent};
pub use policy::{AllowAll, Decision, PlannedCall, Policy, Verdict};
pub use session::SessionHandle;
pub use speed::SpeedSettings;
pub use state::{Observer, TRANSITIONS, TurnState};
pub use stats::{NoStats, RequestStat, StatEvent, Stats, ToolCallStat, ToolStatOutcome, TurnStat};
pub use store::{Attempt, MemorySessionStore, Opened, Record, SessionStore, StoreError};
pub use toolset::ToolSet;
pub use turn::{Session, SessionParts};
pub use verify::{NoVerifier, Verifier, VerifyCx, VerifyOutcome};
