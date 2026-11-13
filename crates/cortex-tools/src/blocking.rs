//! The one way blocking work leaves the core thread
//! (`docs/architecture/02-runtime.md`, "Blocking work never runs on the core
//! thread").
//!
//! File reads, directory walks, regex search, diffing and hashing all block.
//! [`run_blocking`] hands such work to the runtime's bounded blocking pool
//! with a cancellation token, and waits for whichever comes first: the work,
//! or the token.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use cortex_base::CancelToken;

/// Why blocking work has no result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlockingError {
    /// The token fired first. The work was told, and stops at its next
    /// check of the token.
    #[error("cancelled")]
    Cancelled,
    /// The work panicked. The text is the panic's message.
    #[error("the task panicked: {0}")]
    Panicked(String),
}

/// Runs `work` on the blocking pool and waits for it, or for `cancel`.
///
/// `work` is given the token too, and must check it between pieces of work
/// (once a file, once a chunk): when the token fires this function returns
/// [`BlockingError::Cancelled`] at once and no longer waits, and the work
/// goes on only until that next check. Work that is cancelled must leave
/// nothing half-done behind it; the tools here only read.
///
/// A panic in the work is caught and returned, not passed on, so that one
/// tool call cannot take the session down.
///
/// It must be called inside a Tokio runtime.
pub async fn run_blocking<T, F>(cancel: &CancelToken, work: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce(&CancelToken) -> T + Send + 'static,
{
    if cancel.is_cancelled() {
        return Err(BlockingError::Cancelled);
    }
    let token = cancel.clone();
    let mut task = pin!(tokio::task::spawn_blocking(move || work(&token)));
    let mut cancelled = pin!(cancel.cancelled());
    poll_fn(|cx| {
        if let Poll::Ready(joined) = task.as_mut().poll(cx) {
            return Poll::Ready(joined.map_err(|error| {
                if error.is_panic() {
                    BlockingError::Panicked(panic_message(error.into_panic().as_ref()))
                } else {
                    // The runtime is shutting down and dropped the task.
                    BlockingError::Cancelled
                }
            }));
        }
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(BlockingError::Cancelled));
        }
        Poll::Pending
    })
    .await
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "no message".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    #[test]
    fn the_work_runs_and_its_value_comes_back() {
        let cancel = CancelToken::new();
        let value = block_on(run_blocking(&cancel, |token| {
            assert!(!token.is_cancelled());
            6 * 7
        }));
        assert_eq!(value, Ok(42));
    }

    #[test]
    fn a_token_that_has_fired_stops_the_work_before_it_starts() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&ran);
        let outcome = block_on(run_blocking(&cancel, move |_| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        assert_eq!(outcome, Err(BlockingError::Cancelled));
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn cancelling_returns_at_once_and_the_work_sees_the_token() {
        // The work blocks until the test has seen the cancelled outcome,
        // then reports whether its token had fired. Nothing here waits on
        // time: each side waits on the other.
        let cancel = CancelToken::new();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (seen_tx, seen_rx) = mpsc::channel::<bool>();

        let canceller = cancel.clone();
        let trigger = std::thread::spawn(move || {
            started_rx.recv().expect("the work starts");
            canceller.cancel();
        });
        // The runtime outlives the call: dropping a runtime waits for its
        // blocking tasks, and this one is still blocked when the call
        // returns.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let outcome = runtime.block_on(run_blocking(&cancel, move |token| {
            started_tx.send(()).expect("the test is listening");
            release_rx.recv().expect("the test lets go");
            seen_tx
                .send(token.is_cancelled())
                .expect("the test is listening");
        }));
        assert_eq!(outcome, Err(BlockingError::Cancelled));
        trigger.join().expect("the canceller finishes");

        release_tx.send(()).expect("the work is waiting");
        assert!(
            seen_rx.recv().expect("the work reports"),
            "the work saw the token"
        );
    }

    #[test]
    fn a_panic_in_the_work_is_an_error_not_a_panic() {
        let cancel = CancelToken::new();
        let outcome: Result<(), _> =
            block_on(run_blocking(&cancel, |_| panic!("the disk is on fire")));
        assert_eq!(
            outcome,
            Err(BlockingError::Panicked("the disk is on fire".to_owned()))
        );
    }
}
