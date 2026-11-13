//! A future whose panic becomes an error
//! (`docs/architecture/02-runtime.md`, "Panics").
//!
//! A panic is a bug, and it must not take a session down with it. A tool
//! call, a plan and a question each run as a task behind [`Guarded`], so
//! that a panic in one becomes an error result for that call. A turn runs
//! behind it too, so that a panic anywhere else in the loop ends the turn
//! and leaves the session able to take the next prompt.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Runs a future, and gives the panic's message as an error if it panics.
pub(crate) struct Guarded<F>(pub(crate) Pin<Box<F>>);

impl<F: Future> Future for Guarded<F> {
    type Output = Result<F::Output, String>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.0.as_mut();
        // The future is dropped after a panic and never polled again, so
        // whatever state the panic left in it is not observed.
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(context))) {
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "no message".to_owned());
                Poll::Ready(Err(message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::*;

    fn poll<F: Future>(future: &mut Guarded<F>) -> Poll<Result<F::Output, String>> {
        Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn a_panic_is_an_error_with_its_message_and_anything_else_passes_through() {
        let mut fine = Guarded(Box::pin(async { 7 }));
        assert_eq!(poll(&mut fine), Poll::Ready(Ok(7)));

        let mut waits = Guarded(Box::pin(std::future::pending::<u8>()));
        assert_eq!(poll(&mut waits), Poll::Pending);

        let mut breaks = Guarded(Box::pin(async {
            assert!(std::hint::black_box(false), "the wheels came off");
        }));
        let Poll::Ready(Err(message)) = poll(&mut breaks) else {
            panic!("a panic is an error");
        };
        assert!(message.contains("the wheels came off"), "{message}");
    }
}
