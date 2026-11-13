//! Waiting behind a trait, so that every wait can be tested without waiting
//! (`docs/testing-and-ci.md`, "No real waiting in tests").
//!
//! Code that waits (the retry layer between attempts, a read racing a time
//! limit, the process runner between the stages of stopping a command, the
//! core while an approval is open) does not ask the async runtime for a
//! timer. It asks a [`Timer`], and reads the time from a
//! [`Clock`](crate::Clock). The harness gives it the runtime's timer. A test
//! gives it the fake timer of `cortex-testkit`, which moves the fake clock
//! instead of waiting.
//!
//! The trait is here, and not in the crates that wait, since step 13: the
//! provider layer and the process runner each had one, alike to the letter,
//! because neither can see the other. One trait means one fake serves
//! both, and the core can hand one timer to everything it assembles. The
//! runtime's implementation stays with the crates that link the runtime:
//! this crate links none.

use std::pin::Pin;
use std::time::Duration;

/// A wait in progress: a future that completes when the time has passed.
/// Dropping it gives the wait up.
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Something that can wait.
pub trait Timer: Send + Sync + std::fmt::Debug {
    /// A future that completes after `duration`. The wait starts when this
    /// is called, not when the future is first polled.
    fn sleep(&self, duration: Duration) -> Sleep;
}

/// A shared timer.
pub type SharedTimer = std::sync::Arc<dyn Timer>;
