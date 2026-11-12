//! Waiting behind a trait, so that every wait in this crate can be tested
//! without waiting (`docs/testing-and-ci.md`, "No real waiting in tests").
//!
//! The retry layer waits between attempts, and every read of a response
//! races a time limit. None of that code asks the runtime for a timer
//! directly. It asks a [`Timer`], and reads the time from the
//! [`Clock`](cortex_base::Clock) of `cortex-base`. The harness gives it
//! [`TokioTimer`]. A test gives it the fake timer of `cortex-testkit`, which
//! moves the fake clock instead of waiting, so the retry layer's backoff,
//! the idle limit and a ten-minute cooldown are tested in microseconds and
//! against one clock.

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

/// The async runtime's timer: what the harness runs with.
///
/// [`sleep`](Timer::sleep) must be called from inside a Tokio runtime that
/// has its timer enabled, which the core thread's runtime has.
#[cfg(feature = "http")]
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioTimer;

#[cfg(feature = "http")]
impl Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// What ended a wait for some work.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Raced<T> {
    /// The work finished.
    Done(T),
    /// The request was cancelled first.
    Cancelled,
    /// The time limit passed first.
    TimedOut,
}

/// Runs `work` until it finishes, the token is cancelled or `limit`
/// completes, whichever is first.
///
/// Cancellation is looked at first, so cancelled work does not go on. The
/// work is looked at before the limit, so work that is ready is never
/// reported as late.
pub(crate) async fn race<F: Future>(
    work: F,
    cancel: &cortex_base::CancelToken,
    limit: &mut Sleep,
) -> Raced<F::Output> {
    use std::task::Poll;

    let mut work = std::pin::pin!(work);
    let mut cancelled = std::pin::pin!(cancel.cancelled());
    std::future::poll_fn(|context| {
        if cancelled.as_mut().poll(context).is_ready() {
            return Poll::Ready(Raced::Cancelled);
        }
        if let Poll::Ready(output) = work.as_mut().poll(context) {
            return Poll::Ready(Raced::Done(output));
        }
        if limit.as_mut().poll(context).is_ready() {
            return Poll::Ready(Raced::TimedOut);
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll, Waker};

    use cortex_base::CancelToken;

    use super::*;

    fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    fn never() -> Sleep {
        Box::pin(std::future::pending())
    }

    fn at_once() -> Sleep {
        Box::pin(std::future::ready(()))
    }

    #[test]
    fn work_that_is_ready_wins_over_a_limit_that_has_passed() {
        let token = CancelToken::new();
        let mut limit = at_once();
        let mut raced = std::pin::pin!(race(async { 5 }, &token, &mut limit));
        assert_eq!(poll(raced.as_mut()), Poll::Ready(Raced::Done(5)));
    }

    #[test]
    fn the_limit_ends_work_that_is_not_ready() {
        let token = CancelToken::new();
        let mut limit = at_once();
        let mut raced = std::pin::pin!(race(std::future::pending::<u8>(), &token, &mut limit));
        assert_eq!(poll(raced.as_mut()), Poll::Ready(Raced::TimedOut));
    }

    #[test]
    fn cancellation_wins_over_everything() {
        let token = CancelToken::new();
        let mut limit = never();
        {
            let mut raced = std::pin::pin!(race(std::future::pending::<u8>(), &token, &mut limit));
            assert_eq!(poll(raced.as_mut()), Poll::Pending);
            token.cancel();
            assert_eq!(poll(raced.as_mut()), Poll::Ready(Raced::Cancelled));
        }
        let mut limit = at_once();
        let mut raced = std::pin::pin!(race(async { 5 }, &token, &mut limit));
        assert_eq!(poll(raced.as_mut()), Poll::Ready(Raced::Cancelled));
    }
}
