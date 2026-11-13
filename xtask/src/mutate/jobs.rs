//! Pieces of work shared out among the copies of the tree, for `cargo
//! xtask mutate --jobs N`.
//!
//! Each job is a thread with a copy of the tree and a build directory of
//! its own, and a [`Runner`] for them. A job takes the next piece of work
//! when it is free; what a piece came to goes back to the thread that
//! started the jobs, which alone writes the results and the report, in the
//! order the pieces finished. With one job there is no thread: the pieces
//! are done in order, and the time is asked before each, as it always was.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::drive::Runner;
use crate::Result;

/// How often the starting thread asks whether the time is up while every
/// job is busy.
const ASK_EVERY: Duration = Duration::from_millis(250);

/// Does `work` for each item, on the jobs, and hands each result to `done`
/// on this thread. Returns whether every item was done: not when the time
/// ran out (`out_of_time`), which stops new pieces being started while the
/// ones under way finish and are kept. An error from `done` stops the jobs
/// in the same way and is returned once they have.
pub(crate) fn for_each<T, R>(
    runners: &mut [&mut dyn Runner],
    copies: &[PathBuf],
    items: &[T],
    work: &(dyn Fn(&mut dyn Runner, &Path, &T) -> R + Sync),
    out_of_time: &mut dyn FnMut() -> bool,
    done: &mut dyn FnMut(&T, R) -> Result<()>,
) -> Result<bool>
where
    T: Sync,
    R: Send,
{
    if let ([runner], [copy]) = (&mut *runners, copies) {
        for item in items {
            if out_of_time() {
                return Ok(false);
            }
            let outcome = work(&mut **runner, copy, item);
            done(item, outcome)?;
        }
        return Ok(true);
    }

    // Asked once before any job starts, so that a run with no time left
    // starts nothing. After this it is asked as pieces finish, and a job
    // may start one more piece in the moment before the answer reaches it.
    if items.is_empty() {
        return Ok(true);
    }
    if out_of_time() {
        return Ok(false);
    }
    let next = AtomicUsize::new(0);
    let halt = AtomicBool::new(false);
    let (sender, received) = mpsc::channel::<(usize, R)>();
    std::thread::scope(|scope| {
        for (runner, copy) in runners.iter_mut().zip(copies) {
            let (sender, next, halt) = (sender.clone(), &next, &halt);
            scope.spawn(move || {
                loop {
                    if halt.load(Ordering::SeqCst) {
                        return;
                    }
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(item) = items.get(index) else {
                        return;
                    };
                    let outcome = work(&mut **runner, copy, item);
                    if sender.send((index, outcome)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);

        let mut finished = 0;
        let mut failure = None;
        loop {
            if failure.is_none() && !halt.load(Ordering::SeqCst) && out_of_time() {
                halt.store(true, Ordering::SeqCst);
            }
            match received.recv_timeout(ASK_EVERY) {
                Ok((index, outcome)) => {
                    // After a failure, what is still finishing is let go
                    // unrecorded: the run is ending with an error.
                    if failure.is_none() {
                        finished += 1;
                        if let Err(error) = done(&items[index], outcome) {
                            halt.store(true, Ordering::SeqCst);
                            failure = Some(error);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(finished == items.len()),
        }
    })
}

/// The copy of the tree that job `job` works in: `tree` for the first, as
/// a run without `--jobs` uses, and `tree-2`, `tree-3` … for the others.
pub(crate) fn copy(scratch: &Path, job: usize) -> PathBuf {
    scratch.join(numbered("tree", job))
}

/// `name` for the first job, `name-<n>` for the n-th after it.
pub(crate) fn numbered(name: &str, job: usize) -> String {
    if job == 0 {
        name.to_owned()
    } else {
        format!("{name}-{}", job + 1)
    }
}
