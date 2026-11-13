//! The one way blocking work leaves the core thread
//! (`docs/architecture/02-runtime.md`, "Blocking work never runs on the core
//! thread").
//!
//! File reads, directory walks, regex search, diffing and hashing all block.
//! [`run_blocking`] hands such work to the runtime's bounded blocking pool
//! with a cancellation token, and waits for whichever comes first: the work,
//! or the token.
//!
//! # A seeded order, for tests
//!
//! Work on the blocking pool finishes when its thread finishes, which is at
//! no fixed moment for the core thread. A test of the agent loop therefore
//! saw a call's plan arrive before or after the response's next event as the
//! machine happened to run it, and a test that depended on one order failed
//! some runs in ten.
//!
//! With the `test-hooks` feature, [`InlineBlocking`] makes [`run_blocking`]
//! on one thread run its work on that thread, after a number of turns of
//! the runtime that a seeded generator decides. The order of everything a
//! single-threaded test sees is then decided by the seed: the same seed
//! gives the same order on every machine, and another seed gives another.
//! Only a dev-dependency turns the feature on (`cargo xtask rules` checks
//! that), and nothing but a test makes an [`InlineBlocking`].

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
    #[cfg(feature = "test-hooks")]
    if let Some(turns) = inline::turns() {
        return inline::run(cancel, work, turns).await;
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

#[cfg(feature = "test-hooks")]
pub use inline::InlineBlocking;

/// The seeded order of the `test-hooks` feature.
#[cfg(feature = "test-hooks")]
mod inline {
    use std::cell::RefCell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use cortex_base::{CancelToken, Rng, SeededRng};

    use super::{BlockingError, panic_message};

    thread_local! {
        /// The generator that decides when this thread's blocking work
        /// finishes. `None`, as it is everywhere outside a test that asked:
        /// the work goes to the blocking pool.
        static ORDER: RefCell<Option<SeededRng>> = const { RefCell::new(None) };
    }

    /// How many turns of the runtime a piece of work may be made to wait:
    /// mostly few, as real work is quick, and now and then many, so that a
    /// seed also tries the order in which one piece arrives long after the
    /// rest.
    const TURNS: [u32; 8] = [0, 1, 1, 2, 2, 3, 5, 8];

    /// While one of these is alive, [`run_blocking`](super::run_blocking)
    /// called on the thread that made it runs its work on that thread, at a
    /// moment the seed decides, and not on the blocking pool.
    ///
    /// For tests that run a single-threaded runtime on the test's own
    /// thread. With every piece of work on one thread, the order of events
    /// is the same every time for the same seed.
    ///
    /// The work is not run at all when its token fires before its turn
    /// comes. On the blocking pool it would have been started and told to
    /// stop; either way the caller gets [`BlockingError::Cancelled`].
    ///
    /// The work must not wait for another task of the same runtime (a gate
    /// another task opens, a channel another task sends on). On the pool
    /// that task would run and let it go. Here the work is on the only
    /// thread there is, and the task never runs.
    #[derive(Debug)]
    pub struct InlineBlocking {
        /// What was in force before, put back when this is dropped.
        before: Option<SeededRng>,
    }

    impl InlineBlocking {
        /// Starts a seeded order on the calling thread.
        #[must_use = "the order ends when this is dropped"]
        pub fn seeded(seed: u64) -> Self {
            let before = ORDER.with(|order| order.replace(Some(SeededRng::new(seed))));
            Self { before }
        }
    }

    impl Drop for InlineBlocking {
        fn drop(&mut self) {
            let before = self.before.take();
            ORDER.with(|order| *order.borrow_mut() = before);
        }
    }

    /// The turns the next piece of work waits, when a seeded order is in
    /// force on this thread.
    pub(super) fn turns() -> Option<u32> {
        ORDER.with(|order| {
            let mut order = order.borrow_mut();
            let rng = order.as_mut()?;
            let index = usize::try_from(rng.below(TURNS.len() as u64)).unwrap_or(0);
            TURNS.get(index).copied()
        })
    }

    /// Lets everything else that is ready run `turns` times, then runs the
    /// work here.
    pub(super) async fn run<T, F>(
        cancel: &CancelToken,
        work: F,
        turns: u32,
    ) -> Result<T, BlockingError>
    where
        F: FnOnce(&CancelToken) -> T,
    {
        for _ in 0..turns {
            tokio::task::yield_now().await;
            if cancel.is_cancelled() {
                return Err(BlockingError::Cancelled);
            }
        }
        catch_unwind(AssertUnwindSafe(|| work(cancel)))
            .map_err(|panic| BlockingError::Panicked(panic_message(panic.as_ref())))
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

    /// Three pieces of work started together, each noting when it ran,
    /// under a seeded order. Returns the order they ran in and the thread
    /// each ran on.
    #[cfg(feature = "test-hooks")]
    fn seeded_order(seed: u64) -> (Vec<u8>, Vec<std::thread::ThreadId>) {
        use std::sync::{Arc, Mutex};

        let _order = InlineBlocking::seeded(seed);
        let ran: Arc<Mutex<Vec<(u8, std::thread::ThreadId)>>> = Arc::default();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let mut tasks = Vec::new();
            for name in 0..3u8 {
                let ran = Arc::clone(&ran);
                tasks.push(tokio::spawn(async move {
                    run_blocking(&CancelToken::new(), move |_| {
                        ran.lock()
                            .expect("no panic holds it")
                            .push((name, std::thread::current().id()));
                    })
                    .await
                }));
            }
            for task in tasks {
                task.await.expect("the task ends").expect("the work ran");
            }
        });
        let ran = ran.lock().expect("no panic holds it").clone();
        ran.into_iter().unzip()
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn under_a_seeded_order_the_work_runs_on_the_calling_thread() {
        let (order, threads) = seeded_order(7);

        assert_eq!(order.len(), 3);
        assert!(
            threads.iter().all(|id| *id == std::thread::current().id()),
            "{threads:?}"
        );
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn the_same_seed_gives_the_same_order_and_some_seed_gives_another() {
        let orders: Vec<Vec<u8>> = (0..32).map(|seed| seeded_order(seed).0).collect();

        for (seed, order) in (0..32).zip(&orders) {
            assert_eq!(&seeded_order(seed).0, order, "seed {seed}");
        }
        // The order they were started in is one of them, and not the only one.
        assert!(orders.contains(&vec![0, 1, 2]), "{orders:?}");
        assert!(orders.iter().any(|order| order != &[0, 1, 2]), "{orders:?}");
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn when_the_seeded_order_ends_the_work_goes_back_to_the_pool() {
        drop(InlineBlocking::seeded(1));

        let here = std::thread::current().id();
        let there = block_on(run_blocking(&CancelToken::new(), |_| {
            std::thread::current().id()
        }));

        assert_ne!(there, Ok(here));
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn an_order_inside_another_gives_the_outer_one_back() {
        let _outer = InlineBlocking::seeded(1);
        drop(InlineBlocking::seeded(2));

        let here = std::thread::current().id();
        let there = block_on(run_blocking(&CancelToken::new(), |_| {
            std::thread::current().id()
        }));

        assert_eq!(there, Ok(here));
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn under_a_seeded_order_a_token_that_fires_first_wins_and_the_work_never_runs() {
        // Seed after seed, until one makes the work wait a turn: the token
        // is fired by a task that runs in that turn.
        let mut cancelled = 0;
        for seed in 0..32 {
            let _order = InlineBlocking::seeded(seed);
            let cancel = CancelToken::new();
            let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = std::sync::Arc::clone(&ran);
            let canceller = cancel.clone();
            let outcome = block_on(async {
                let work = tokio::spawn(async move {
                    run_blocking(&cancel, move |_| {
                        flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    })
                    .await
                });
                tokio::spawn(async move { canceller.cancel() });
                work.await.expect("the task ends")
            });
            let ran = ran.load(std::sync::atomic::Ordering::SeqCst);
            match outcome {
                Ok(()) => assert!(ran, "seed {seed}"),
                Err(error) => {
                    assert_eq!(error, BlockingError::Cancelled, "seed {seed}");
                    assert!(!ran, "seed {seed}: cancelled work must not have run");
                    cancelled += 1;
                }
            }
        }
        assert!(cancelled > 0, "no seed in 32 made the work wait");
        assert!(cancelled < 32, "no seed in 32 ran the work at once");
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn under_a_seeded_order_a_panic_in_the_work_is_still_an_error() {
        let _order = InlineBlocking::seeded(3);
        let outcome: Result<(), _> = block_on(run_blocking(&CancelToken::new(), |_| {
            panic!("the disk is on fire")
        }));

        assert_eq!(
            outcome,
            Err(BlockingError::Panicked("the disk is on fire".to_owned()))
        );
    }
}
