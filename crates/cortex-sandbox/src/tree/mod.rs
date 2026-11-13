//! Process trees: a command started so that it, and everything it starts,
//! can be stopped as one
//! (`docs/decisions/0030-process-tree-control-in-cortex-sandbox.md`).
//!
//! | | Linux | Windows |
//! |---|---|---|
//! | [`Tree::spawn`] | A new session and process group. The command is told to die if the harness does, and the [`Guard`] is told the group. | A new process group inside a Job Object that kills its members when it is closed. |
//! | [`Tree::interrupt`] | `SIGINT` to the group | A Ctrl-Break event to the group |
//! | [`Tree::terminate`] | `SIGTERM` to the group | Not there: [`io::ErrorKind::Unsupported`] |
//! | [`Tree::kill`] | `SIGKILL` to the group | The Job Object is terminated |
//! | [`Tree::wait`] | Blocks until the command itself has exited | The same |
//! | [`Tree::others_running`] | Counts the group's other members in the process table | Asks the Job Object for its members, and leaves out the command and any that have exited |
//! | [`Tree::end`], and dropping the last handle | Whatever is left of the tree is killed | The same |
//!
//! # What it promises, and what it does not
//!
//! - **Nothing of the tree outlives its handle.** [`Tree::end`] kills what
//!   is left, and so does dropping the last clone.
//! - **Nothing of the tree outlives the harness.** On Windows the Job
//!   Object's handle closes with the process, however the process ends. On
//!   Linux the kernel signals the command itself, and the [`Guard`] kills
//!   the rest of the group.
//! - **A signal never reaches a stranger.** On Linux the group's id is the
//!   command's process id, which the system may give to another process
//!   once the command has been reaped. So the command is waited for
//!   without being reaped, and is reaped only in [`Tree::end`], after the
//!   last signal. That needs exited children to be kept until they are
//!   waited for, which a process that ignores `SIGCHLD` does not get:
//!   [`Tree::spawn`] puts that signal back to its default if it finds it
//!   ignored.
//! - **On Linux a process that leaves the group is not followed.** One that
//!   starts a session of its own is outside the tree, and outlives it.
//!   Confining such a process is the sandbox's job. On Windows nothing
//!   leaves a Job Object.
//!
//! [`Output`] is the pipe such a command writes to, read by a thread that
//! can be told to stop.
//!
//! Everything here blocks, and none of it for long except [`Tree::wait`]
//! and [`Output::read`], which a caller runs on threads of their own.

#![allow(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    reason = "this is where processes are started; every call here is short, \
              except `wait`, which is documented as blocking"
)]

mod guard;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::io;
use std::process::Command;
use std::sync::Arc;

pub use guard::{Guard, run_guard};

#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exit {
    /// It exited with this code. On Windows a command that was killed has
    /// the code its killer chose; [`Tree::kill`] chooses 1.
    Code(i32),
    /// A signal ended it (Unix only): its number.
    Signal(i32),
}

impl Exit {
    /// Whether the command exited with code 0.
    #[must_use]
    pub fn success(self) -> bool {
        self == Self::Code(0)
    }
}

/// A command and everything it started. Clones are handles to the one tree.
#[derive(Debug, Clone)]
pub struct Tree {
    inner: Arc<imp::Inner>,
}

impl Tree {
    /// Starts `command` as the first process of a tree of its own.
    ///
    /// The standard streams, the environment and the working directory are
    /// the caller's to set on `command` first. This sets what makes the
    /// tree: the session and the death signal on Linux, the process group
    /// and the Job Object on Windows, where it replaces any creation flags
    /// the caller set. The command is taken whole and dropped once the
    /// process runs, so that the harness keeps no end of a pipe it gave it.
    ///
    /// With a `guard`, the tree is also killed if the harness dies before
    /// it has ended ([`Guard`]). If the guard cannot be told, the command is
    /// killed again and this fails: a tree is never left unguarded.
    ///
    /// **Call it from a thread that lives as long as the command may.** On
    /// Linux the death signal is sent when the *thread* that started the
    /// command ends, so a command started from a pool thread dies when the
    /// pool lets that thread go. The harness's core thread is the right one.
    pub fn spawn(command: Command, guard: Option<&Guard>) -> io::Result<Self> {
        let inner = imp::Inner::spawn(command, guard)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// The process id of the command itself.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.inner.id()
    }

    /// Asks the whole tree, politely, to stop. A program may ignore it.
    ///
    /// Fails when the request cannot be made: the tree has ended, or on
    /// Windows the command itself has exited or there is no console to send
    /// the event through. The caller then goes on to the next stage.
    pub fn interrupt(&self) -> io::Result<()> {
        self.inner.interrupt()
    }

    /// Tells the whole tree to stop. A program may still ignore it.
    ///
    /// Windows has no such stage between the event and the kill: there
    /// this fails with [`io::ErrorKind::Unsupported`].
    pub fn terminate(&self) -> io::Result<()> {
        self.inner.terminate()
    }

    /// Kills the whole tree. Nothing can ignore it.
    ///
    /// The tree is still there to [`wait`](Self::wait) for and to
    /// [`end`](Self::end).
    pub fn kill(&self) -> io::Result<()> {
        self.inner.kill()
    }

    /// Blocks until the command itself has exited, and says how. What the
    /// command started may still be running.
    ///
    /// It may be called from several threads, and again after it has
    /// returned.
    pub fn wait(&self) -> io::Result<Exit> {
        self.inner.wait()
    }

    /// How many processes of the tree are running, not counting the
    /// command itself. After the command has exited, this is what it left
    /// behind: background processes, which the tree's end will kill.
    ///
    /// On Linux it reads the process table and counts the members of the
    /// command's process group: a process that left the group is not
    /// counted. Reading the whole table takes a few microseconds for each
    /// process on the machine, so for a command that is under a second
    /// old only the processes with a higher id than the command's are
    /// read, which is where its group's members are. On Windows it asks
    /// the Job Object for its members by process id, and counts the ones
    /// that are not the command and have not exited: a job lists a process
    /// for a moment after its exit, so its own count of active processes
    /// says that a command which has just exited left itself running.
    pub fn others_running(&self) -> io::Result<usize> {
        self.inner.others_running()
    }

    /// Ends the tree: kills whatever is left of it, and waits for the
    /// command itself to be gone. After this the tree takes no signal and
    /// counts nothing: those calls fail. [`wait`](Self::wait) still says
    /// how the command ended, and [`id`](Self::id) what its id was.
    ///
    /// Dropping the last handle does the same.
    pub fn end(&self) {
        self.inner.end();
    }
}

/// The reading end of the one pipe a command writes both its standard
/// output and its standard error to, so that the two arrive in the order
/// they were written.
#[derive(Debug)]
pub struct Output {
    pipe: imp::OutputPipe,
}

/// Stops a read of an [`Output`] from another thread.
#[derive(Debug, Clone)]
pub struct OutputStop {
    handle: imp::StopHandle,
}

/// How one read of an [`Output`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// This many bytes were read.
    Bytes(usize),
    /// Every process that could write has closed the pipe or is gone.
    Closed,
    /// [`OutputStop::stop`] was called.
    Stopped,
}

impl Output {
    /// Makes the pipe and gives its writing end to `command` as its
    /// standard output and its standard error. Call it before
    /// [`Tree::spawn`].
    pub fn attach(command: &mut Command) -> io::Result<(Self, OutputStop)> {
        let (reader, writer) = io::pipe()?;
        command.stdout(writer.try_clone()?);
        command.stderr(writer);
        let (pipe, handle) = imp::OutputPipe::new(reader)?;
        Ok((Self { pipe }, OutputStop { handle }))
    }

    /// Reads what the tree has written, blocking until there is something,
    /// until the pipe closes, or until the read is stopped.
    ///
    /// The pipe closes when no process holds its writing end. A process
    /// the command left running holds it, which is why a reader can be
    /// stopped.
    pub fn read(&mut self, buffer: &mut [u8]) -> io::Result<ReadOutcome> {
        self.pipe.read(buffer)
    }
}

impl OutputStop {
    /// Makes the read in progress, and every later one, end as
    /// [`ReadOutcome::Stopped`].
    ///
    /// On Linux a blocked read returns at once. On Windows it is cancelled,
    /// and a read that was just about to start when this was called is not:
    /// it blocks until the pipe closes, or until this is called again. So
    /// a caller that must have the reader back calls it again after a
    /// moment, as the process runner does, and killing the tree closes the
    /// pipe in any case: nothing leaves a Job Object.
    pub fn stop(&self) {
        self.handle.stop();
    }
}

/// How many of a Job Object's members are processes the command left
/// running: every member but the command itself and those that have
/// already exited.
///
/// The job lists a process for a moment after it has exited, the command
/// included, so neither a count of the members nor "the command has
/// exited, so every member is another" can be trusted. It is here, and not
/// with the Windows code that asks the job, so that it is built and tested
/// on every system.
#[cfg(any(windows, test))]
fn others_among(members: &[u32], command: u32, has_exited: impl Fn(u32) -> bool) -> usize {
    members
        .iter()
        .filter(|member| **member != command && !has_exited(**member))
        .count()
}

/// Whether a process that could not be opened by its id is gone: only when
/// Windows said that no process has the id (`ERROR_INVALID_PARAMETER`, 87).
/// A process that refuses to be opened is still there.
#[cfg(any(windows, test))]
fn unopened_is_gone(code: Option<i32>) -> bool {
    /// `ERROR_INVALID_PARAMETER`.
    const NO_SUCH_PROCESS: i32 = 87;
    code == Some(NO_SUCH_PROCESS)
}

/// The error of a call on a tree that has ended.
fn ended() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "the process tree has ended")
}

#[cfg(test)]
mod tests {
    use super::{others_among, unopened_is_gone};

    #[test]
    fn a_process_that_refuses_to_be_opened_is_still_there() {
        // No process has the id: it is gone.
        assert!(unopened_is_gone(Some(87)));
        // Access denied (5), or anything else: it is there, and counted.
        assert!(!unopened_is_gone(Some(5)));
        assert!(!unopened_is_gone(Some(0)));
        assert!(!unopened_is_gone(None));
    }

    #[test]
    fn the_command_itself_is_never_one_of_the_processes_it_left() {
        // The job still lists the command in the moment after its exit,
        // whether the system says it has exited or not.
        assert_eq!(others_among(&[40], 40, |_| true), 0);
        assert_eq!(others_among(&[40], 40, |_| false), 0);
        assert_eq!(others_among(&[], 40, |_| false), 0);
        // What it started and is still running is counted.
        assert_eq!(others_among(&[40, 44, 48], 40, |_| false), 2);
        assert_eq!(others_among(&[44], 40, |_| false), 1);
    }

    #[test]
    fn a_member_that_has_exited_is_not_left_running() {
        // A child that ended a moment before the command did is still
        // listed too.
        assert_eq!(others_among(&[40, 44, 48], 40, |member| member == 44), 1);
        assert_eq!(others_among(&[44, 48], 40, |_| true), 0);
    }
}
