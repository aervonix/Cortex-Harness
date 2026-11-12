//! Locking that does not panic.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks a mutex, whether or not a thread panicked while holding it.
///
/// Every mutex in this crate guards a value that is valid after each
/// statement that changes it, so a poisoned lock holds nothing broken. Going
/// on is better than turning one panic into a panic in every other thread
/// that touches a cancellation token or the clock.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
