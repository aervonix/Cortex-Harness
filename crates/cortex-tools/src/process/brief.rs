//! A short program run for its answer: a shell asked for its version, `git`
//! asked what has changed.
//!
//! This is not how a model's command runs. It is for the harness's own
//! questions to programs it chose itself: no process tree, no artifact, no
//! staging, and a limit on how long its answer may take and how large it
//! may be. A program that runs over either is killed, and the answer is
//! that there is none.
//!
//! It blocks, and is called off the core thread.

#![allow(
    clippy::disallowed_methods,
    reason = "runs on the blocking pool, and waits for a program with a limit"
)]

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The most a brief program may print before its answer is given up.
const MOST_BYTES: u64 = 8 * 1024 * 1024;

/// Why a brief program gave no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unanswered {
    /// The program could not be started: it is not there.
    NotStarted,
    /// It ran and did not exit with success.
    Failed,
    /// It printed more than an answer may be.
    TooMuch,
    /// It had not answered when the limit passed, and was killed.
    TooSlow,
}

/// Runs `program` with `arguments` and returns what it wrote to its
/// standard output, if it exited with success within `limit`.
///
/// Its standard error is thrown away and its input is closed. `env` is
/// added to the inherited environment.
pub(crate) fn run(
    program: impl AsRef<OsStr>,
    arguments: &[&str],
    cwd: Option<&Path>,
    env: &[(&str, &str)],
    limit: Duration,
) -> Result<Vec<u8>, Unanswered> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let child = command.spawn().map_err(|_| Unanswered::NotStarted)?;
    answer(child, limit)
}

/// What a program already started wrote to its standard output, which
/// must be a pipe, if it exited with success within `limit`: for one its
/// caller started its own way (the harness's git, which
/// `cortex_base::git::start` starts). A program that runs over the limit,
/// or prints too much, is killed.
pub(crate) fn answer(mut child: Child, limit: Duration) -> Result<Vec<u8>, Unanswered> {
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Unanswered::Failed);
    };
    // The read is on a thread of its own, so that this one can stop
    // waiting for it: a pipe has no time limit of its own.
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("cortex-brief-output".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let read = stdout.take(MOST_BYTES + 1).read_to_end(&mut bytes);
            let _ = sender.send(read.map(|_| bytes));
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Unanswered::Failed);
    }
    let answer = match receiver.recv_timeout(limit) {
        Ok(Ok(bytes)) if bytes.len() as u64 <= MOST_BYTES => Ok(bytes),
        Ok(Ok(_)) => Err(Unanswered::TooMuch),
        Ok(Err(_)) => Err(Unanswered::Failed),
        Err(_) => Err(Unanswered::TooSlow),
    };
    if answer.is_err() {
        let _ = child.kill();
    }
    // The output has ended, or the program was killed. A program exits a
    // moment after it closes its output, and this waits for that moment.
    let status = child.wait().map_err(|_| Unanswered::Failed)?;
    match answer {
        Ok(_) if !status.success() => Err(Unanswered::Failed),
        answer => answer,
    }
}
