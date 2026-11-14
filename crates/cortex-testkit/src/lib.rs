//! Test support for the whole workspace (`docs/testing-and-ci.md`). A
//! dev-dependency only: nothing here is part of a shipped build.
//!
//! | Module | What it gives |
//! |---|---|
//! | re-exports | [`FakeClock`] and [`SeededRng`], which live in `cortex-base` beside the traits they implement. |
//! | [`cases`] | Property tests without a property-testing crate. |
//! | [`canary`] | [`assert_never_printed`]: no `Debug` or `Display` form of a value holds a made-up key. |
//! | [`timer`] | [`FakeTimer`]: waits that move the fake clock instead of waiting. |
//! | [`gate`] | [`Gate`]: how two sides of a test take turns without sleeping. |
//! | [`scratch`] | [`Scratch`]: a directory of a test's own, with files, a git repository when the test needs one, and nothing left behind. |
//! | [`seed`] | The seed of a test whose order of events is seeded: 0 in an ordinary run, the next one for each repeat of `cargo xtask stress`, and printed so that a failure can be run again. |
//! | [`children`] | Child processes for process tests: the test binary run again as a child, a grandchild or a child that will not stop, and a [`Roll`](children::Roll) that shows each one start and die. |
//! | [`rerun`] | [`Rerun`](rerun::Rerun): the running test again in a child process, with a `PATH`, a working directory and variables of its own, for tests of how a program is found. |
//! | [`exec`] | [`block_on`], which runs async code with no runtime, and [`join`], which runs two futures together. |
//! | [`provider`] | [`ScriptedProvider`]: a model that says what the test tells it to. For tests of the agent loop. |
//! | [`transport`] | [`ScriptedTransport`]: recorded responses in place of a network. For tests of protocols and of the client. |
//! | [`protocol`] | [`TestProtocol`]: a wire dialect made for tests. |
//! | [`server`] | [`SseServer`]: a provider's HTTP endpoint on the loopback interface. For tests of the real HTTP client, and of the real binary. |
//! | [`fixtures`] | Made-up models with every shape of speed tier, and requests to them. |
//! | [`profile`] | Where a test's time went, by phase and wait, with `CORTEX_TESTKIT_PROFILE=1`. Nothing when it is off. |
//! | [`watch`] | What a test was doing when it ran out of time: its waits and its process tree, written to its standard error before nextest stops it. And `CORTEX_TESTKIT_PAUSE_MS`, which widens the scripted server's races on purpose. |
//! | [`fixture`] | Recorded provider responses: [`Fixture`](fixture::Fixture), the files one is made of, and [`Scrubber`](fixture::Scrubber), which takes credentials and identifiers out of a recording and refuses one that holds the key. |
//! | [`vterm`] | [`VTerm`]: a virtual terminal, and a [`Report`](vterm::Report) of what each frame did to it. For tests of the presenter, and to parse a program's output. |
//! | [`pty`] | [`pty::Session`]: a program run in a pseudo-terminal (ConPTY on Windows), its output recorded and parsed, keys typed into it. |
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

pub mod canary;
pub mod cases;
pub mod children;
pub mod exec;
pub mod fixture;
pub mod fixtures;
pub mod gate;
pub mod profile;
pub mod protocol;
pub mod provider;
pub mod pty;
pub mod rerun;
pub mod schema;
pub mod scratch;
pub mod seed;
pub mod server;
pub mod timer;
pub mod transport;
pub mod vterm;
pub mod watch;

pub use canary::{Printed, assert_never_printed};
pub use cortex_base::{Clock, FakeClock, Rng, SeededRng};
pub use exec::{block_on, join};
pub use gate::Gate;
pub use protocol::TestProtocol;
pub use provider::{Response, ScriptedProvider};
pub use scratch::Scratch;
pub use server::{ReceivedRequest, ServerReply, SseServer};
pub use timer::FakeTimer;
pub use transport::{Reply, ScriptedTransport};
pub use vterm::VTerm;
