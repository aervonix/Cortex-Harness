//! The fake timer: waits that take no time, on the fake clock
//! (`docs/testing-and-ci.md`, "No real waiting in tests").
//!
//! Code in `cortex-llm` waits on a [`Timer`] and reads the time from a
//! [`Clock`](cortex_base::Clock). A test gives it a [`FakeClock`] and a
//! [`FakeTimer`] made from the same clock, and then a wait and the clock
//! agree: after "waiting" 30 seconds, the clock reads 30 seconds later.
//!
//! There are two kinds, for two kinds of test.
//!
//! - [`FakeTimer::auto`]: a wait finishes the first time it is polled, and
//!   moves the clock forward by its length. Code that waits and goes on
//!   (a retry's backoff) runs straight through, and the test reads back
//!   what it waited for with [`FakeTimer::sleeps`]. A wait that is raced
//!   against work that is ready is never polled, and so never happens.
//! - [`FakeTimer::manual`]: a wait finishes only when the test moves the
//!   clock past its end with [`FakeTimer::advance`]. This is for a time
//!   limit that must **not** pass until the test says so: an idle timeout
//!   raced against a stream that has stalled.

use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use cortex_base::{Clock, FakeClock};
use cortex_llm::{Sleep, Timer};

/// A [`Timer`] that moves a [`FakeClock`] instead of waiting. Clones share
/// one state.
#[derive(Debug, Clone)]
pub struct FakeTimer {
    clock: FakeClock,
    auto: bool,
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    /// The waits that finished, in the order they finished.
    slept: Vec<Duration>,
    /// The waits that are running (manual timers only).
    running: Vec<Running>,
    next_id: u64,
    /// Tasks waiting for a wait to start.
    watchers: Vec<Waker>,
}

#[derive(Debug)]
struct Running {
    id: u64,
    length: Duration,
    /// When it ends, on the fake clock's monotonic time.
    ends: Duration,
    waker: Option<Waker>,
}

impl FakeTimer {
    /// A timer whose waits finish as soon as they are polled, moving
    /// `clock` forward by their length.
    #[must_use]
    pub fn auto(clock: &FakeClock) -> Self {
        Self::new(clock, true)
    }

    /// A timer whose waits finish only when [`advance`](Self::advance)
    /// moves `clock` past their end.
    #[must_use]
    pub fn manual(clock: &FakeClock) -> Self {
        Self::new(clock, false)
    }

    fn new(clock: &FakeClock, auto: bool) -> Self {
        Self {
            clock: clock.clone(),
            auto,
            state: Arc::default(),
        }
    }

    /// Moves the clock forward and finishes every wait whose time has come.
    pub fn advance(&self, duration: Duration) {
        self.clock.advance(duration);
        let now = self.clock.monotonic();
        let due: Vec<Waker> = self
            .lock()
            .running
            .iter_mut()
            .filter(|wait| wait.ends <= now)
            .filter_map(|wait| wait.waker.take())
            .collect();
        for waker in due {
            waker.wake();
        }
    }

    /// The length of every wait that has finished, in order.
    #[must_use]
    pub fn sleeps(&self) -> Vec<Duration> {
        self.lock().slept.clone()
    }

    /// The lengths of the waits that are running now. Always empty for an
    /// automatic timer, whose waits finish when they start.
    #[must_use]
    pub fn running(&self) -> Vec<Duration> {
        self.lock().running.iter().map(|wait| wait.length).collect()
    }

    /// Completes when a wait is running: at once if one is, otherwise when
    /// the next one starts. A test of a time limit awaits this before it
    /// moves the clock, so that it moves it for a limit that exists.
    #[must_use]
    pub fn sleeping(&self) -> Sleeping {
        Sleeping {
            timer: self.clone(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Timer for FakeTimer {
    fn sleep(&self, duration: Duration) -> Sleep {
        if self.auto {
            return Box::pin(AutoSleep {
                timer: self.clone(),
                length: duration,
                done: false,
            });
        }
        let (id, watchers) = {
            let mut state = self.lock();
            let id = state.next_id;
            state.next_id += 1;
            state.running.push(Running {
                id,
                length: duration,
                ends: self.clock.monotonic().saturating_add(duration),
                waker: None,
            });
            (id, std::mem::take(&mut state.watchers))
        };
        for waker in watchers {
            waker.wake();
        }
        Box::pin(ManualSleep {
            timer: self.clone(),
            id,
        })
    }
}

/// A wait of an automatic timer.
struct AutoSleep {
    timer: FakeTimer,
    length: Duration,
    done: bool,
}

impl Future for AutoSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        if !self.done {
            self.done = true;
            self.timer.clock.advance(self.length);
            let length = self.length;
            self.timer.lock().slept.push(length);
        }
        Poll::Ready(())
    }
}

/// A wait of a manual timer.
struct ManualSleep {
    timer: FakeTimer,
    id: u64,
}

impl Future for ManualSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let now = self.timer.clock.monotonic();
        let mut state = self.timer.lock();
        let Some(index) = state.running.iter().position(|wait| wait.id == self.id) else {
            // Finished on an earlier poll.
            return Poll::Ready(());
        };
        if state.running[index].ends <= now {
            let wait = state.running.swap_remove(index);
            state.slept.push(wait.length);
            Poll::Ready(())
        } else {
            state.running[index].waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

impl Drop for ManualSleep {
    fn drop(&mut self) {
        // A wait that was given up is not running any more.
        self.timer.lock().running.retain(|wait| wait.id != self.id);
    }
}

/// The future of [`FakeTimer::sleeping`].
#[derive(Debug)]
pub struct Sleeping {
    timer: FakeTimer,
}

impl Future for Sleeping {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut state = self.timer.lock();
        if state.running.is_empty() {
            state.watchers.push(context.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn poll<F: Future + ?Sized>(future: &mut Pin<Box<F>>) -> Poll<F::Output> {
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn an_automatic_wait_moves_the_clock_when_it_is_polled() {
        let clock = FakeClock::default();
        let timer = FakeTimer::auto(&clock);
        let mut wait = timer.sleep(secs(30));
        let unpolled = timer.sleep(secs(999));
        assert_eq!(
            clock.monotonic(),
            Duration::ZERO,
            "making a wait is not waiting"
        );

        assert_eq!(poll(&mut wait), Poll::Ready(()));
        assert_eq!(poll(&mut wait), Poll::Ready(()), "and only once");
        assert_eq!(clock.monotonic(), secs(30));
        assert_eq!(clock.now().to_string(), "2026-01-01T00:00:30Z");
        assert_eq!(timer.sleeps(), [secs(30)]);
        assert!(timer.running().is_empty());

        drop(unpolled);
        assert_eq!(
            timer.sleeps(),
            [secs(30)],
            "a wait that lost its race never happened"
        );
    }

    #[test]
    fn a_manual_wait_ends_when_the_clock_is_moved_past_it() {
        let clock = FakeClock::default();
        let timer = FakeTimer::manual(&clock);
        let mut short = timer.sleep(secs(10));
        let mut long = timer.sleep(secs(90));
        assert_eq!(timer.running(), [secs(10), secs(90)]);
        assert_eq!(poll(&mut short), Poll::Pending);
        assert_eq!(poll(&mut long), Poll::Pending);

        timer.advance(secs(9));
        assert_eq!(poll(&mut short), Poll::Pending);
        timer.advance(secs(1));
        assert_eq!(poll(&mut short), Poll::Ready(()));
        assert_eq!(poll(&mut long), Poll::Pending);
        assert_eq!(timer.sleeps(), [secs(10)]);
        assert_eq!(timer.running(), [secs(90)]);

        // Moving the clock itself counts too: the timer reads it.
        clock.advance(secs(80));
        assert_eq!(poll(&mut long), Poll::Ready(()));
        assert_eq!(timer.sleeps(), [secs(10), secs(90)]);
        assert_eq!(clock.monotonic(), secs(90));
    }

    #[test]
    fn a_wait_that_is_given_up_is_not_running() {
        let clock = FakeClock::default();
        let timer = FakeTimer::manual(&clock);
        let wait = timer.sleep(secs(60));
        assert_eq!(timer.running().len(), 1);
        drop(wait);
        assert!(timer.running().is_empty());
        assert!(timer.sleeps().is_empty());
    }

    #[test]
    fn a_test_can_wait_for_a_wait_to_start() {
        let clock = FakeClock::default();
        let timer = FakeTimer::manual(&clock);
        let mut sleeping = Box::pin(timer.sleeping());
        assert_eq!(poll(&mut sleeping), Poll::Pending);
        let _wait = timer.sleep(secs(5));
        assert_eq!(poll(&mut sleeping), Poll::Ready(()));
        assert_eq!(poll(&mut Box::pin(timer.sleeping())), Poll::Ready(()));
    }
}
