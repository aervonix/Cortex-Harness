//! The guard: a small process that outlives the harness for a moment, to
//! kill the process trees the harness left behind.
//!
//! Linux can tell one child to die with its parent, and no more: what that
//! child started lives on. So the harness keeps one guard process, holds
//! the writing end of a pipe to it, and tells it the process group of every
//! tree it starts and of every tree that has ended. The guard does nothing
//! until the pipe closes, which the kernel does when the harness is gone,
//! however it went. Then it kills the groups it still knows, and exits.
//!
//! - **It kills only what it was told.** A group it was not told about, its
//!   own group, and the ids that mean "everyone" are never signalled.
//! - **It outlives what kills the harness.** It runs in a session of its
//!   own, so nothing aimed at the harness's terminal or process group
//!   reaches it; it ignores the signals a harness is stopped with; and on
//!   Linux its name in the process table is `cx-tree-guard`, so that a
//!   kill of every process named like the harness passes it by. A kill
//!   that names the guard itself, or matches whole command lines, takes
//!   it, and then only the command itself dies with the harness.
//! - **It is the harness's own program** under a hidden subcommand, like
//!   the sandbox launcher, so there is nothing to install. The binary's
//!   side of that is [`run_guard`].
//!
//! Windows needs none of this: a Job Object is closed, and its processes
//! killed, by the system when the harness's process ends. There a
//! [`Guard`] is accepted and does nothing.

use std::ffi::OsString;
use std::path::PathBuf;

/// How to reach the guard process. Clones share the one process.
///
/// The process is started the first time a tree is spawned with the guard,
/// and started again if it is found gone.
#[derive(Debug, Clone)]
pub struct Guard {
    #[cfg(unix)]
    shared: std::sync::Arc<imp::Shared>,
}

impl Guard {
    /// A guard that runs `program` with `args`, which must end up in
    /// [`run_guard`]: the harness's own executable and its hidden
    /// subcommand.
    #[must_use]
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        let program = program.into();
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        #[cfg(unix)]
        {
            Self {
                shared: std::sync::Arc::new(imp::Shared::new(program, args)),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (program, args);
            Self {}
        }
    }
}

/// What the guard process does: the body of the hidden subcommand.
///
/// It reads its standard input until that closes, then kills the process
/// groups it was told of and not told to forget, and returns. It prints
/// nothing.
pub fn run_guard() {
    #[cfg(unix)]
    imp::run();
    #[cfg(not(unix))]
    {
        // Nothing starts a guard on this system. If something does, it
        // waits for its pipe to close like the real one, and does nothing.
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
    }
}

#[cfg(unix)]
pub(super) use imp::Watch;

#[cfg(unix)]
mod imp {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::io::{self, BufRead, Write};
    use std::path::PathBuf;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::super::unix;
    use super::Guard;

    /// What the clones of a [`Guard`] share.
    #[derive(Debug)]
    pub(in super::super) struct Shared {
        program: PathBuf,
        args: Vec<OsString>,
        state: Mutex<State>,
    }

    #[derive(Debug, Default)]
    struct State {
        running: Option<Running>,
        /// The groups of the trees that have not ended.
        watched: BTreeSet<u32>,
    }

    #[derive(Debug)]
    struct Running {
        child: Child,
        pipe: ChildStdin,
    }

    /// The guard's knowledge of one tree. The tree gives it up when it
    /// ends, before its group's id can be anyone else's.
    #[derive(Debug)]
    pub(in super::super) struct Watch {
        guard: Guard,
        group: u32,
    }

    impl Shared {
        pub(super) fn new(program: PathBuf, args: Vec<OsString>) -> Self {
            Self {
                program,
                args,
                state: Mutex::default(),
            }
        }

        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn start(&self) -> io::Result<Running> {
            let mut command = Command::new(&self.program);
            command
                .args(&self.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            unix::in_own_session(&mut command);
            let mut child = command.spawn()?;
            let Some(pipe) = child.stdin.take() else {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("the guard has no pipe"));
            };
            Ok(Running { child, pipe })
        }
    }

    impl State {
        fn tell(&mut self, line: &str) -> io::Result<()> {
            let Some(running) = &mut self.running else {
                return Err(io::ErrorKind::NotConnected.into());
            };
            // A guard that has died may not have closed its pipe in the
            // system's eyes yet, and a line written to it then is lost
            // without an error. Its own exit is the plain sign.
            if !matches!(running.child.try_wait(), Ok(None)) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            running.pipe.write_all(line.as_bytes())?;
            running.pipe.flush()
        }

        /// Puts away a guard that no longer listens.
        fn discard(&mut self) {
            if let Some(mut gone) = self.running.take() {
                // It is our own child, so the id is its own until the wait.
                let _ = gone.child.kill();
                let _ = gone.child.wait();
            }
        }
    }

    impl Guard {
        /// Tells the guard of a tree's group, starting the guard if it is
        /// not running.
        pub(in super::super) fn watch(&self, group: u32) -> io::Result<Watch> {
            let mut state = self.shared.lock();
            state.watched.insert(group);
            if state.tell(&format!("+{group}\n")).is_err() {
                // It was never started, or it is gone. Start one, and tell
                // it every tree that is still running.
                state.discard();
                let told = self.shared.start().and_then(|running| {
                    state.running = Some(running);
                    let all: String = state
                        .watched
                        .iter()
                        .map(|group| format!("+{group}\n"))
                        .collect();
                    state.tell(&all)
                });
                if let Err(error) = told {
                    state.discard();
                    state.watched.remove(&group);
                    return Err(error);
                }
            }
            Ok(Watch {
                guard: self.clone(),
                group,
            })
        }
    }

    impl Watch {
        /// Tells the guard to forget the group. A guard that is gone has
        /// forgotten it already.
        pub(in super::super) fn release(&self) {
            let mut state = self.guard.shared.lock();
            if state.watched.remove(&self.group) {
                let _ = state.tell(&format!("-{}\n", self.group));
            }
        }
    }

    impl Drop for Shared {
        fn drop(&mut self) {
            // Every tree holds a clone of the guard, so none is left: the
            // guard finds nothing to kill when its pipe closes, and exits.
            let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
            if let Some(Running { mut child, pipe }) = state.running.take() {
                drop(pipe);
                let _ = child.wait();
            }
        }
    }

    /// The guard process.
    pub(super) fn run() {
        unix::ignore_stop_signals();
        unix::name_the_guard();
        let own = u32::try_from(unix::own_group()).unwrap_or(0);
        let left = follow(io::stdin().lock(), own);
        for group in left {
            if let Ok(group) = libc::pid_t::try_from(group) {
                let _ = unix::signal_group(group, libc::SIGKILL);
            }
        }
    }

    /// Reads the harness's lines until the pipe closes, and returns the
    /// groups that are still to be killed.
    ///
    /// A line is `+` or `-` and a group's id in decimal. Anything else is
    /// passed over, and so is an id that is not a command's group: 0 and 1,
    /// and the guard's own. A read that fails ends it like a closed pipe.
    pub(super) fn follow(input: impl BufRead, own: u32) -> BTreeSet<u32> {
        let mut groups = BTreeSet::new();
        for line in input.lines() {
            let Ok(line) = line else { break };
            let (add, digits) = match line.as_bytes().first() {
                Some(b'+') => (true, &line[1..]),
                Some(b'-') => (false, &line[1..]),
                _ => continue,
            };
            if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let Ok(group) = digits.parse::<u32>() else {
                continue;
            };
            if group <= 1 || group == own || libc::pid_t::try_from(group).is_err() {
                continue;
            }
            if add {
                groups.insert(group);
            } else {
                groups.remove(&group);
            }
        }
        groups
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn left(lines: &str) -> Vec<u32> {
            follow(lines.as_bytes(), 77).into_iter().collect()
        }

        #[test]
        fn it_remembers_what_it_was_told_and_forgets_what_ended() {
            assert_eq!(left(""), Vec::<u32>::new());
            assert_eq!(left("+100\n+200\n"), vec![100, 200]);
            assert_eq!(left("+100\n+200\n-100\n"), vec![200]);
            assert_eq!(left("+100\n-100\n+100\n"), vec![100]);
            // A last line with no line break still counts: the harness may
            // die in the middle of a write, and a group told is a group.
            assert_eq!(left("+100\n+200"), vec![100, 200]);
        }

        #[test]
        fn it_never_takes_an_id_that_is_not_a_commands_group() {
            // Everyone, the caller's own group, the first process, and the
            // guard's own group.
            assert_eq!(left("+0\n+1\n+77\n+-1\n+-5\n"), Vec::<u32>::new());
            // Ids no process can have.
            assert_eq!(
                left("+4294967295\n+99999999999999999999\n"),
                Vec::<u32>::new()
            );
        }

        #[test]
        fn it_passes_over_what_it_cannot_read() {
            assert_eq!(
                left("hello\n\n+\n+12a\n+ 12\n*12\n+0x10\n+500\n-\n-abc\n"),
                vec![500]
            );
            // Bytes that are not text end the reading, and what was told
            // before them stands.
            let mut input = b"+300\n".to_vec();
            input.extend_from_slice(&[0xff, 0xfe, b'\n']);
            input.extend_from_slice(b"+400\n");
            assert_eq!(
                follow(input.as_slice(), 77).into_iter().collect::<Vec<_>>(),
                vec![300]
            );
        }
    }
}
