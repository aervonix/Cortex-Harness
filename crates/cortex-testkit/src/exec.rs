//! Running async code in a test with no async runtime.
//!
//! Most of what `cortex-llm` does is async and needs no runtime: its waits
//! are on a [`Timer`](cortex_llm::Timer), and its streams are polled by
//! whoever reads them. [`block_on`] runs such code to its end on the test's
//! own thread. A test that needs real sockets uses a real runtime instead.

use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::Thread;

/// Wakes the thread that is running [`block_on`].
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs a future to its end on the calling thread and returns its output.
///
/// Between polls the thread is parked until something wakes the future, so
/// a future that waits for another thread (a [`Gate`](crate::Gate) opened by
/// a scripted server) works. A future that nothing will ever wake blocks
/// for good, and the test runner's time limit ends the test.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            // A wake that came during the poll makes this return at once.
            Poll::Pending => std::thread::park(),
        }
    }
}

/// Runs two futures together on one task and returns both outputs. For a
/// test that must do something while the code under test is waiting: the
/// code is one future, and what the test does is the other.
///
/// The first future is polled before the second.
pub async fn join<A: Future, B: Future>(first: A, second: B) -> (A::Output, B::Output) {
    let mut first = std::pin::pin!(first);
    let mut second = std::pin::pin!(second);
    let mut first_output = None;
    let mut second_output = None;
    std::future::poll_fn(|context| {
        if first_output.is_none()
            && let Poll::Ready(output) = first.as_mut().poll(context)
        {
            first_output = Some(output);
        }
        if second_output.is_none()
            && let Poll::Ready(output) = second.as_mut().poll(context)
        {
            second_output = Some(output);
        }
        if first_output.is_some() && second_output.is_some() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    match (first_output, second_output) {
        (Some(first), Some(second)) => (first, second),
        _ => unreachable!("both futures finished before the wait ended"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gate;

    #[test]
    fn two_futures_run_together_and_the_first_is_polled_first() {
        let gate = Gate::new();
        let opener = gate.clone();
        // The first waits for what the second does.
        let (first, second) = block_on(join(
            async {
                gate.opened().await;
                "waited"
            },
            async move {
                opener.open();
                "opened"
            },
        ));
        assert_eq!((first, second), ("waited", "opened"));
        assert_eq!(block_on(join(async { 1 }, async { 2 })), (1, 2));
    }

    #[test]
    fn it_runs_a_future_that_is_ready_and_one_that_waits_for_a_thread() {
        assert_eq!(block_on(async { 6 * 7 }), 42);

        let gate = Gate::new();
        let opener = gate.clone();
        let thread = std::thread::spawn(move || opener.open());
        block_on(gate.opened());
        thread.join().unwrap();
    }
}
