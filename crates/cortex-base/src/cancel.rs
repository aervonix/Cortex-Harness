//! The tree of cancellation tokens (`docs/architecture/02-runtime.md`,
//! "Cancellation").
//!
//! ```text
//! process ─► session ─► turn ─┬─► model request
//!                             ├─► tool call A
//!                             └─► tool call B ─► child process group
//! ```
//!
//! Cancelling a token cancels every token below it, and nothing above or
//! beside it. Async code races each wait that can take time against its
//! token ([`CancelToken::race`]). Blocking code asks between pieces of work
//! ([`CancelToken::is_cancelled`]).
//!
//! The tokens are built on the standard library's wakers and depend on no
//! async runtime, so the same token works on the core thread, on the
//! blocking pool and in a test with no runtime at all.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use crate::sync::lock;

/// A handle to one node of the cancellation tree.
///
/// Clones are the same node: cancelling one cancels them all. A
/// [`child`](CancelToken::child) is a new node below this one.
///
/// Cancellation is permanent. There is no way to reset a token: make a new
/// child for the next piece of work.
#[derive(Clone)]
pub struct CancelToken {
    node: Arc<Node>,
}

struct Node {
    /// Set once, under the lock, and read without it.
    cancelled: AtomicBool,
    links: Mutex<Links>,
}

#[derive(Default)]
struct Links {
    /// The nodes below this one. A node whose last handle is gone is
    /// removed the next time the list is tidied.
    children: Vec<Weak<Node>>,
    /// When `children` reaches this length it is tidied.
    tidy_at: usize,
    /// The tasks waiting for this node to be cancelled, by registration.
    waiters: Vec<(u64, Waker)>,
    next_waiter: u64,
}

impl CancelToken {
    /// A new root: a token that nothing above can cancel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            node: Arc::new(Node {
                cancelled: AtomicBool::new(false),
                links: Mutex::new(Links::default()),
            }),
        }
    }

    /// A new token below this one. It is cancelled when this one is, and can
    /// be cancelled alone without touching this one. A child of a cancelled
    /// token is cancelled from the start.
    #[must_use]
    pub fn child(&self) -> Self {
        let child = Self::new();
        let mut links = lock(&self.node.links);
        // The flag is only ever set under this lock, so a child is either
        // registered before the cancel walks the list, or born cancelled.
        if self.node.cancelled.load(Ordering::Acquire) {
            child.node.cancelled.store(true, Ordering::Release);
        } else {
            if links.children.len() >= links.tidy_at {
                links.children.retain(|child| child.strong_count() > 0);
                links.tidy_at = (links.children.len() * 2).max(8);
            }
            links.children.push(Arc::downgrade(&child.node));
        }
        child
    }

    /// Cancels this token and every token below it. Waiting tasks are woken.
    /// Cancelling again does nothing.
    pub fn cancel(&self) {
        // Walked with a list, not by recursion, so a deep tree cannot
        // overflow the stack.
        let mut pending = vec![Arc::clone(&self.node)];
        while let Some(node) = pending.pop() {
            let (children, waiters) = {
                let mut links = lock(&node.links);
                if node.cancelled.swap(true, Ordering::AcqRel) {
                    continue;
                }
                (
                    std::mem::take(&mut links.children),
                    std::mem::take(&mut links.waiters),
                )
            };
            // Woken outside the lock: a waker may run code of its own.
            for (_, waker) in waiters {
                waker.wake();
            }
            pending.extend(children.iter().filter_map(Weak::upgrade));
        }
    }

    /// Whether this token, or any token above it, has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.node.cancelled.load(Ordering::Acquire)
    }

    /// A future that completes when this token is cancelled, at once if it
    /// already is. It never completes otherwise.
    #[must_use]
    pub fn cancelled(&self) -> Cancelled {
        Cancelled {
            node: Arc::clone(&self.node),
            registration: None,
        }
    }

    /// Runs `future` until it finishes or this token is cancelled, whichever
    /// is first. `None` means cancelled: the future was dropped where it
    /// stood, so it must be safe to drop at any of its await points.
    ///
    /// A token that is already cancelled wins even against a future that is
    /// ready, so cancelled work does not start.
    pub async fn race<F: Future>(&self, future: F) -> Option<F::Output> {
        let mut future = std::pin::pin!(future);
        let mut cancelled = std::pin::pin!(self.cancelled());
        std::future::poll_fn(|context| {
            if cancelled.as_mut().poll(context).is_ready() {
                return Poll::Ready(None);
            }
            future.as_mut().poll(context).map(Some)
        })
        .await
    }

    /// A guard that cancels this token when it is dropped: at the end of a
    /// scope, on an early return, or when a panic unwinds through it.
    #[must_use = "the token is cancelled as soon as the guard is dropped"]
    pub fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop {
            token: Some(self.clone()),
        }
    }

    /// How many tokens directly below this one are still held somewhere.
    #[cfg(test)]
    fn live_children(&self) -> usize {
        lock(&self.node.links)
            .children
            .iter()
            .filter(|child| child.strong_count() > 0)
            .count()
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// The future returned by [`CancelToken::cancelled`].
pub struct Cancelled {
    node: Arc<Node>,
    /// The number this future is registered under while it waits.
    registration: Option<u64>,
}

impl Future for Cancelled {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.node.cancelled.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        let node = Arc::clone(&self.node);
        let mut links = lock(&node.links);
        // Checked again under the lock: `cancel` sets the flag and takes the
        // waiters while holding it, so a waker stored here is not missed.
        if node.cancelled.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        let registered = self.registration.and_then(|number| {
            links
                .waiters
                .iter_mut()
                .find(|(waiter, _)| *waiter == number)
        });
        match registered {
            Some((_, waker)) => waker.clone_from(context.waker()),
            None => {
                let number = links.next_waiter;
                links.next_waiter += 1;
                links.waiters.push((number, context.waker().clone()));
                self.registration = Some(number);
            }
        }
        Poll::Pending
    }
}

impl Drop for Cancelled {
    fn drop(&mut self) {
        if let Some(number) = self.registration {
            lock(&self.node.links)
                .waiters
                .retain(|(waiter, _)| *waiter != number);
        }
    }
}

impl std::fmt::Debug for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cancelled")
            .field("cancelled", &self.node.cancelled.load(Ordering::Acquire))
            .finish()
    }
}

/// Cancels its token when dropped. Made by [`CancelToken::cancel_on_drop`].
#[derive(Debug)]
pub struct CancelOnDrop {
    /// `None` once the guard has been disarmed.
    token: Option<CancelToken>,
}

impl CancelOnDrop {
    /// Ends the guard without cancelling its token.
    pub fn disarm(mut self) {
        self.token = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = &self.token {
            token.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::task::Wake;

    use super::*;

    /// A waker that counts how often it was woken.
    #[derive(Default)]
    struct Counter(AtomicUsize);

    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Counter {
        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Polls a future once with a counting waker.
    fn poll_once<F: Future + Unpin>(future: &mut F, counter: &Arc<Counter>) -> Poll<F::Output> {
        let waker = Waker::from(Arc::clone(counter));
        Pin::new(future).poll(&mut Context::from_waker(&waker))
    }

    fn waiters(token: &CancelToken) -> usize {
        lock(&token.node.links).waiters.len()
    }

    #[test]
    fn cancelling_a_node_cancels_everything_below_it_and_nothing_else() {
        let process = CancelToken::new();
        let session = process.child();
        let other_session = process.child();
        let turn = session.child();
        let request = turn.child();
        let tool_a = turn.child();
        let tool_b = turn.child();
        let children_of_b = tool_b.child();

        turn.cancel();

        for (name, token) in [
            ("turn", &turn),
            ("request", &request),
            ("tool a", &tool_a),
            ("tool b", &tool_b),
            ("tool b's children", &children_of_b),
        ] {
            assert!(token.is_cancelled(), "{name} should be cancelled");
        }
        for (name, token) in [
            ("process", &process),
            ("session", &session),
            ("the other session", &other_session),
        ] {
            assert!(!token.is_cancelled(), "{name} should not be cancelled");
        }

        // The next turn is a new child, and is not cancelled.
        let next_turn = session.child();
        assert!(!next_turn.is_cancelled());
        process.cancel();
        assert!(next_turn.is_cancelled());
        assert!(other_session.is_cancelled());
    }

    #[test]
    fn a_leaf_can_be_cancelled_alone() {
        let turn = CancelToken::new();
        let tool_a = turn.child();
        let tool_b = turn.child();
        tool_a.cancel();
        assert!(tool_a.is_cancelled());
        assert!(!tool_b.is_cancelled());
        assert!(!turn.is_cancelled());
    }

    #[test]
    fn clones_are_the_same_node() {
        let token = CancelToken::new();
        let clone = token.clone();
        let child = clone.child();
        token.cancel();
        token.cancel();
        assert!(clone.is_cancelled());
        assert!(child.is_cancelled());
        assert_eq!(format!("{token:?}"), "CancelToken { cancelled: true }");
    }

    #[test]
    fn a_child_of_a_cancelled_token_is_born_cancelled() {
        let token = CancelToken::default();
        token.cancel();
        let child = token.child();
        assert!(child.is_cancelled());
        assert!(child.child().is_cancelled());
        assert_eq!(token.live_children(), 0, "nothing is kept for it");
    }

    #[test]
    fn a_waiting_task_is_woken_exactly_when_its_token_is_cancelled() {
        let turn = CancelToken::new();
        let tool = turn.child();
        let counter = Arc::new(Counter::default());
        let mut wait = tool.cancelled();

        assert_eq!(poll_once(&mut wait, &counter), Poll::Pending);
        assert_eq!(poll_once(&mut wait, &counter), Poll::Pending);
        assert_eq!(waiters(&tool), 1, "polling again does not register again");
        assert_eq!(counter.count(), 0);

        turn.cancel();
        assert_eq!(counter.count(), 1, "woken once, through the parent");
        assert_eq!(poll_once(&mut wait, &counter), Poll::Ready(()));
        assert_eq!(poll_once(&mut wait, &counter), Poll::Ready(()));
    }

    #[test]
    fn a_future_that_moves_to_another_task_wakes_the_new_one() {
        let token = CancelToken::new();
        let first = Arc::new(Counter::default());
        let second = Arc::new(Counter::default());
        let mut wait = token.cancelled();
        assert_eq!(poll_once(&mut wait, &first), Poll::Pending);
        assert_eq!(poll_once(&mut wait, &second), Poll::Pending);
        token.cancel();
        assert_eq!((first.count(), second.count()), (0, 1));
    }

    #[test]
    fn every_waiter_is_woken() {
        let token = CancelToken::new();
        let counters: Vec<_> = (0..5).map(|_| Arc::new(Counter::default())).collect();
        let mut waits: Vec<_> = counters.iter().map(|_| token.cancelled()).collect();
        for (wait, counter) in waits.iter_mut().zip(&counters) {
            assert_eq!(poll_once(wait, counter), Poll::Pending);
        }
        token.cancel();
        assert!(counters.iter().all(|counter| counter.count() == 1));
        assert_eq!(waiters(&token), 0);
    }

    #[test]
    fn a_dropped_wait_leaves_nothing_behind() {
        let token = CancelToken::new();
        let counter = Arc::new(Counter::default());
        let mut kept = token.cancelled();
        assert_eq!(poll_once(&mut kept, &counter), Poll::Pending);
        for _ in 0..100 {
            let mut wait = token.cancelled();
            assert_eq!(poll_once(&mut wait, &counter), Poll::Pending);
        }
        assert_eq!(waiters(&token), 1);
        drop(kept);
        assert_eq!(waiters(&token), 0);
        token.cancel();
        assert_eq!(counter.count(), 0, "nobody was left to wake");
    }

    #[test]
    fn waiting_on_a_cancelled_token_is_ready_at_once() {
        let token = CancelToken::new();
        token.cancel();
        let counter = Arc::new(Counter::default());
        let mut wait = token.cancelled();
        assert_eq!(poll_once(&mut wait, &counter), Poll::Ready(()));
        assert_eq!(waiters(&token), 0);
        assert!(format!("{wait:?}").contains("cancelled: true"));
    }

    #[test]
    fn a_race_ends_with_the_work_or_with_the_cancellation() {
        let counter = Arc::new(Counter::default());

        // The work finishes first.
        let token = CancelToken::new();
        let mut race = Box::pin(token.race(async { 7 }));
        assert_eq!(poll_once(&mut race, &counter), Poll::Ready(Some(7)));

        // The work never finishes, and the token is cancelled.
        let token = CancelToken::new();
        let mut race = Box::pin(token.race(std::future::pending::<u8>()));
        assert_eq!(poll_once(&mut race, &counter), Poll::Pending);
        assert_eq!(counter.count(), 0);
        token.cancel();
        assert_eq!(counter.count(), 1);
        assert_eq!(poll_once(&mut race, &counter), Poll::Ready(None));
        drop(race);
        assert_eq!(waiters(&token), 0);

        // Already cancelled: the work is not polled at all.
        let token = CancelToken::new();
        token.cancel();
        let polled = AtomicBool::new(false);
        let mut race = Box::pin(token.race(async {
            polled.store(true, Ordering::SeqCst);
        }));
        assert_eq!(poll_once(&mut race, &counter), Poll::Ready(None));
        assert!(!polled.load(Ordering::SeqCst));
    }

    #[test]
    fn dropped_children_do_not_pile_up() {
        let session = CancelToken::new();
        let kept = session.child();
        for _ in 0..10_000 {
            let turn = session.child();
            let _tool = turn.child();
        }
        let listed = lock(&session.node.links).children.len();
        assert!(listed <= 16, "{listed} dead children are still listed");
        assert_eq!(session.live_children(), 1);
        session.cancel();
        assert!(kept.is_cancelled());
    }

    #[test]
    fn a_deep_tree_is_cancelled_without_recursion() {
        let root = CancelToken::new();
        let mut chain = vec![root.child()];
        for _ in 0..50_000 {
            let next = chain[chain.len() - 1].child();
            chain.push(next);
        }
        root.cancel();
        assert!(chain.iter().all(CancelToken::is_cancelled));
    }

    #[test]
    fn a_guard_cancels_when_it_is_dropped_and_not_when_disarmed() {
        let token = CancelToken::new();
        {
            let _guard = token.child().cancel_on_drop();
        }
        assert!(!token.is_cancelled(), "the guard held a child");

        let child = token.child();
        drop(child.cancel_on_drop());
        assert!(child.is_cancelled());

        let child = token.child();
        child.cancel_on_drop().disarm();
        assert!(!child.is_cancelled());
        assert_eq!(Arc::strong_count(&child.node), 1, "the guard let go");

        let child = token.child();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = child.cancel_on_drop();
            panic!("the task failed");
        }));
        assert!(unwound.is_err());
        assert!(child.is_cancelled(), "a panic cancels what the task owned");
    }

    #[test]
    fn cancelling_from_another_thread_wakes_a_waiter() {
        let token = CancelToken::new();
        let counter = Arc::new(Counter::default());
        let mut wait = token.child().cancelled();
        assert_eq!(poll_once(&mut wait, &counter), Poll::Pending);

        let remote = token.clone();
        std::thread::spawn(move || remote.cancel())
            .join()
            .expect("the cancelling thread finishes");

        assert_eq!(counter.count(), 1);
        assert_eq!(poll_once(&mut wait, &counter), Poll::Ready(()));
    }

    #[test]
    fn children_made_while_another_thread_cancels_are_never_missed() {
        // Whichever way each race falls, a child made from the token is
        // cancelled once the cancel has returned.
        for _ in 0..200 {
            let token = CancelToken::new();
            let maker = token.clone();
            let made =
                std::thread::spawn(move || (0..50).map(|_| maker.child()).collect::<Vec<_>>());
            token.cancel();
            let children = made.join().expect("the making thread finishes");
            assert!(children.iter().all(CancelToken::is_cancelled));
        }
    }
}
