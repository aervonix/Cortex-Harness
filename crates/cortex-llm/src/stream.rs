//! Streams written as ordinary async code.
//!
//! A response is a stream of events, and the code that produces one is a
//! loop with waits in it: send, read a chunk, decode, wait, send again.
//! [`Generated`] lets that loop be written as an `async` block that hands
//! each item to an [`Emitter`], and turns it into a stream.
//!
//! - **It runs only when asked.** [`Emitter::emit`] hands over one item and
//!   then waits until the reader asks for the next. Nothing is produced
//!   ahead of the reader, so a slow reader holds no queue, and the order in
//!   which things happen is the order the reader pulls them.
//! - **It needs no runtime.** Nothing is spawned. The producing code is
//!   polled by whoever polls the stream.
//! - **Dropping the stream stops the producer** where it stands, which
//!   drops everything it holds: for a response, the connection.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use futures_core::Stream;

use crate::event::ModelEvent;

/// The items a producer has handed over and the reader has not taken yet.
/// It holds one item at a time, except while a producer hands over several
/// without waiting.
type Queue<T> = Arc<Mutex<VecDeque<T>>>;

fn push<T>(queue: &Queue<T>, item: T) {
    queue
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push_back(item);
}

fn pop<T>(queue: &Queue<T>) -> Option<T> {
    queue
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .pop_front()
}

/// The producing side of a [`Generated`] stream.
pub struct Emitter<T> {
    queue: Queue<T>,
}

impl<T> Emitter<T> {
    /// Hands one item to the reader, and returns when the reader asks for
    /// the next one.
    pub async fn emit(&self, item: T) {
        push(&self.queue, item);
        YieldOnce(false).await;
    }
}

impl<T> std::fmt::Debug for Emitter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Emitter")
    }
}

/// Pending once, then ready. It does not wake itself: the reader polls the
/// producer again when it wants the next item.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

/// A stream whose items are produced by a piece of async code.
pub struct Generated<T> {
    queue: Queue<T>,
    /// The producing code. `None` once it has finished.
    producer: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

impl<T: Send + 'static> Generated<T> {
    /// A stream of what `producer` hands to its [`Emitter`]. The stream
    /// ends when the producer returns.
    pub fn new<F, Fut>(producer: F) -> Self
    where
        F: FnOnce(Emitter<T>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let queue: Queue<T> = Arc::default();
        let emitter = Emitter {
            queue: Arc::clone(&queue),
        };
        Self {
            queue,
            producer: Some(Box::pin(producer(emitter))),
        }
    }
}

impl<T> Stream for Generated<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<T>> {
        loop {
            if let Some(item) = pop(&self.queue) {
                return Poll::Ready(Some(item));
            }
            let Some(producer) = self.producer.as_mut() else {
                return Poll::Ready(None);
            };
            match producer.as_mut().poll(context) {
                // It may have handed over a last item before it returned.
                Poll::Ready(()) => self.producer = None,
                Poll::Pending => {
                    return match pop(&self.queue) {
                        Some(item) => Poll::Ready(Some(item)),
                        None => Poll::Pending,
                    };
                }
            }
        }
    }
}

impl<T> std::fmt::Debug for Generated<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generated")
            .field("finished", &self.producer.is_none())
            .finish()
    }
}

/// The response to one model request, as the agent loop reads it: a stream
/// of [`ModelEvent`]s.
///
/// **How it ends.** The last event of every stream is
/// [`ModelEvent::End`] or [`ModelEvent::Error`], exactly one of them, and
/// nothing follows it. A cancelled request ends with an `Error` whose kind
/// is `Interrupted`.
///
/// **Dropping it** abandons the request: the connection is closed and no
/// retry is made.
pub struct ModelStream {
    inner: Pin<Box<dyn Stream<Item = ModelEvent> + Send>>,
}

impl ModelStream {
    /// Wraps a stream of events.
    pub fn new(stream: impl Stream<Item = ModelEvent> + Send + 'static) -> Self {
        Self {
            inner: Box::pin(stream),
        }
    }

    /// A response produced by a piece of async code that hands each event
    /// to the [`Emitter`] it is given.
    pub fn generate<F, Fut>(producer: F) -> Self
    where
        F: FnOnce(Emitter<ModelEvent>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::new(Generated::new(producer))
    }

    /// A response that is these events and nothing else.
    #[must_use]
    pub fn from_events(events: Vec<ModelEvent>) -> Self {
        Self::generate(move |out| async move {
            for event in events {
                out.emit(event).await;
            }
        })
    }

    /// The next event. `None` after the last one.
    pub async fn next(&mut self) -> Option<ModelEvent> {
        std::future::poll_fn(|context| self.inner.as_mut().poll_next(context)).await
    }

    /// Reads the stream to its end and returns every event.
    pub async fn collect(mut self) -> Vec<ModelEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.next().await {
            events.push(event);
        }
        events
    }
}

impl Stream for ModelStream {
    type Item = ModelEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<ModelEvent>> {
        self.inner.as_mut().poll_next(context)
    }
}

impl std::fmt::Debug for ModelStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModelStream")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Waker;

    use super::*;

    fn poll_once<S: Stream + Unpin>(stream: &mut S) -> Poll<Option<S::Item>> {
        Pin::new(stream).poll_next(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn items_arrive_in_order_and_the_stream_ends_with_the_producer() {
        let mut stream = Generated::new(|out| async move {
            for number in 1..=3 {
                out.emit(number).await;
            }
        });
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(1)));
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(2)));
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(3)));
        assert_eq!(poll_once(&mut stream), Poll::Ready(None));
        assert_eq!(poll_once(&mut stream), Poll::Ready(None), "it stays ended");
    }

    #[test]
    fn the_producer_runs_only_as_far_as_the_reader_has_asked() {
        let reached = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&reached);
        let mut stream = Generated::new(move |out| async move {
            for step in 1..=3 {
                seen.store(step, Ordering::SeqCst);
                out.emit(step).await;
            }
            seen.store(99, Ordering::SeqCst);
        });
        assert_eq!(reached.load(Ordering::SeqCst), 0, "nothing runs unasked");
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(1)));
        assert_eq!(reached.load(Ordering::SeqCst), 1);
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(2)));
        assert_eq!(reached.load(Ordering::SeqCst), 2);
        drop(stream);
        assert_eq!(
            reached.load(Ordering::SeqCst),
            2,
            "dropping the stream stops the producer where it stood"
        );
    }

    #[test]
    fn a_producer_that_waits_makes_the_stream_wait() {
        let mut stream = Generated::new(|out| async move {
            out.emit(1).await;
            std::future::pending::<()>().await;
        });
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(1)));
        assert_eq!(poll_once(&mut stream), Poll::Pending);
    }

    #[test]
    fn an_item_handed_over_just_before_the_end_is_not_lost() {
        // The producer pushes without waiting for the reader, then returns.
        let mut stream = Generated::new(|out: Emitter<u8>| async move {
            push(&out.queue, 7);
            push(&out.queue, 8);
        });
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(7)));
        assert_eq!(poll_once(&mut stream), Poll::Ready(Some(8)));
        assert_eq!(poll_once(&mut stream), Poll::Ready(None));
    }
}
