//! Test support for the whole workspace (`docs/testing-and-ci.md`). A
//! dev-dependency only: nothing here is part of a shipped build.
//!
//! | Module | What it gives |
//! |---|---|
//! | re-exports | [`FakeClock`] and [`SeededRng`], which live in `cortex-base` beside the traits they implement. |
//! | [`cases`] | Property tests without a property-testing crate. |
//! | [`timer`] | [`FakeTimer`]: waits that move the fake clock instead of waiting. |
//! | [`gate`] | [`Gate`]: how two sides of a test take turns without sleeping. |
//! | [`children`] | Child processes for process tests: the test binary run again as a child, a grandchild or a child that will not stop, and a [`Roll`](children::Roll) that shows each one start and die. |
//! | [`exec`] | [`block_on`], which runs async code with no runtime, and [`join`], which runs two futures together. |
//! | [`provider`] | [`ScriptedProvider`]: a model that says what the test tells it to. For tests of the agent loop. |
//! | [`transport`] | [`ScriptedTransport`]: recorded responses in place of a network. For tests of protocols and of the client. |
//! | [`protocol`] | [`TestProtocol`]: a wire dialect made for tests. |
//! | [`server`] | [`SseServer`]: a provider's HTTP endpoint on the loopback interface. For tests of the real HTTP client, and of the real binary. |
//! | [`fixtures`] | Made-up models with every shape of speed tier, and requests to them. |
//!
//! The virtual terminal, the pseudo-terminal driver and temporary
//! workspaces arrive with the steps that first need them.
//!
//! # No real waiting
//!
//! Nothing here sleeps. Time is the [`FakeClock`]; a wait is a
//! [`FakeTimer`]'s; an order between two threads is a [`Gate`]. The limits
//! that appear on blocking waits are bounds that turn a mistake into a
//! failed test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test support fails by panicking, with a message that says what was expected"
)]

pub mod cases;
pub mod children;
pub mod exec;
pub mod fixtures;
pub mod gate;
pub mod protocol;
pub mod provider;
pub mod server;
pub mod timer;
pub mod transport;

pub use cortex_base::{Clock, FakeClock, Rng, SeededRng};
pub use exec::{block_on, join};
pub use gate::Gate;
pub use protocol::TestProtocol;
pub use provider::{Response, ScriptedProvider};
pub use server::{ReceivedRequest, ServerReply, SseServer};
pub use timer::FakeTimer;
pub use transport::{Reply, ScriptedTransport};
