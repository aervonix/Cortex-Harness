//! A gate: something one side of a test opens and the other side waits for.
//!
//! Tests never wait for a fixed time (`docs/testing-and-ci.md`, "No real
//! waiting in tests"). Where two things must happen in an order (the server
//! has seen the client hang up, a scripted response may go on) one side
//! opens a gate and the other waits for it. Async code waits with
//! [`Gate::opened`], which costs nothing until the gate opens. A thread
//! waits with [`Gate::wait`], which takes a deadline so that a mistake is a
//! failed test and not a hung one.

use std::panic::Location;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// A gate that starts closed and is opened once. Clones are the same gate.
///
/// A gate is known by where it was made: a test that runs out of time
/// while something waits for one says "the gate made at
/// `crates/…/tests/it/x.rs:40:16`" ([`watch`](crate::watch)).
#[derive(Clone)]
pub struct Gate {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    opened: Condvar,
    made_at: &'static Location<'static>,
}

impl Default for Gate {
    #[track_caller]
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct State {
    open: bool,
    waiting: Vec<Waker>,
}

impl Gate {
    /// A closed gate.
    #[must_use]
    #[track_caller]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                opened: Condvar::new(),
                made_at: Location::caller(),
            }),
        }
    }

    /// "the gate made at <file>:<line>:<column>": how a report names it.
    #[must_use]
    pub fn name(&self) -> String {
        format!("the gate made at {}", self.inner.made_at)
    }

    /// Opens the gate. Everything waiting for it goes on. Opening it again
    /// does nothing.
    pub fn open(&self) {
        let waiting = {
            let mut state = self.lock();
            state.open = true;
            std::mem::take(&mut state.waiting)
        };
        self.inner.opened.notify_all();
        for waker in waiting {
            waker.wake();
        }
    }

    /// Whether the gate has been opened.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.lock().open
    }

    /// Completes when the gate is open. For async code.
    #[must_use]
    pub fn opened(&self) -> Opened {
        Opened { gate: self.clone() }
    }

    /// Blocks the calling thread until the gate is open, or until `limit`
    /// has passed. Returns whether it opened. For a thread that is not
    /// running async code: a scripted server, or a test after its runtime
    /// has finished.
    #[must_use]
    pub fn wait(&self, limit: Duration) -> bool {
        let _doing = crate::watch::doing(format!("waits for {}, up to {limit:?}", self.name()));
        let state = self.lock();
        let (state, _timed_out) = self
            .inner
            .opened
            .wait_timeout_while(state, limit, |state| !state.open)
            .unwrap_or_else(PoisonError::into_inner);
        state.open
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gate")
            .field("open", &self.is_open())
            .field("made_at", &self.inner.made_at)
            .finish()
    }
}

/// The future of [`Gate::opened`].
#[derive(Debug)]
pub struct Opened {
    gate: Gate,
}

impl Future for Opened {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut state = self.gate.lock();
        if state.open {
            return Poll::Ready(());
        }
        if !state
            .waiting
            .iter()
            .any(|waker| waker.will_wake(context.waker()))
        {
            state.waiting.push(context.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gate_opens_once_for_everyone_waiting() {
        let gate = Gate::new();
        assert!(!gate.is_open());
        assert!(
            !gate.wait(Duration::ZERO),
            "a closed gate does not let a thread through"
        );

        let mut first = std::pin::pin!(gate.opened());
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(first.as_mut().poll(&mut context), Poll::Pending);

        gate.clone().open();
        gate.open();
        assert!(gate.is_open());
        assert_eq!(first.as_mut().poll(&mut context), Poll::Ready(()));
        assert!(gate.wait(Duration::ZERO));
        assert!(format!("{gate:?}").contains("true"));
    }

    #[test]
    fn a_gate_is_named_by_where_it_was_made() {
        let (line, gate) = (line!(), Gate::new());
        let name = gate.name();
        assert!(name.starts_with("the gate made at "), "{name}");
        assert!(name.contains("gate.rs"), "{name}");
        assert!(name.contains(&format!(":{line}:")), "{name}");
        // A clone is the same gate, and so is one made by `default`.
        assert_eq!(gate.clone().name(), name);
        let (line, other) = (line!(), Gate::default());
        assert!(
            other.name().contains(&format!(":{line}:")),
            "{}",
            other.name()
        );
    }

    #[test]
    fn a_thread_waits_for_another_to_open_it() {
        let gate = Gate::new();
        let opener = gate.clone();
        let thread = std::thread::spawn(move || opener.open());
        assert!(
            gate.wait(Duration::from_secs(30)),
            "the limit is only a bound"
        );
        thread.join().unwrap();
    }
}
