//! The Unix half of [`super`]: sessions, process groups and signals.
//!
//! The standard library can start a process in a group of its own and can
//! kill one process. It cannot start a session, ask for the death signal,
//! signal a group, wait for a process without reaping it, or wait on two
//! pipes at once. Those five go through `libc` here, and every block says
//! why it is sound.

#![allow(
    unsafe_code,
    reason = "decision 0030: sessions, group signals and the death signal are only \
              reachable through the C library; cortex-sandbox is where such calls live"
)]

use std::io::{self, PipeReader, PipeWriter, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::guard::{Guard, Watch};
use super::{Exit, Limits, ReadOutcome, ended};

/// How much memory the machine has. Linux trees have no memory limit, so
/// it is not asked.
pub(super) fn installed_memory() -> Option<u64> {
    None
}

/// What the child finds in place of the guard's pipe when there is no
/// guard: no file descriptor is negative.
const NO_PIPE: i32 = -1;

/// One tree: the command, and how far along it is.
#[derive(Debug)]
pub(super) struct Inner {
    /// The command's process id, which is also its session's and its
    /// group's.
    pid: libc::pid_t,
    state: Mutex<State>,
    /// How the command ended, once known. The lock is held by the thread
    /// that is waiting for it, so that the command is never reaped under a
    /// waiter.
    exit: Mutex<Option<Exit>>,
    /// When the command was started: how old the tree is decides how much
    /// of the process table a count of it has to read.
    started: std::time::Instant,
    /// The guard's knowledge of this group, given up when the tree ends.
    watch: Option<Watch>,
}

#[derive(Debug)]
struct State {
    child: Child,
    /// Whether the command has been reaped. From then on its process id
    /// may belong to anyone, and no signal is sent.
    ended: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing here panics while it holds a lock, and a tree must still be
    // killable if something did.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Inner {
    pub(super) fn spawn(
        mut command: Command,
        guard: Option<&Guard>,
        _limits: Limits,
    ) -> io::Result<Self> {
        keep_exited_children();
        let harness = libc::pid_t::try_from(std::process::id())
            .map_err(|_| io::Error::other("the process id does not fit"))?;
        // The guard's pipe, for the child to announce itself on. It is
        // known only once the guard is ready, and is another if the guard
        // has to be started again, so the child reads it from here.
        let guard_pipe = Arc::new(AtomicI32::new(NO_PIPE));
        let for_child = Arc::clone(&guard_pipe);
        // SAFETY: the closure runs in the child between `fork` and `exec`,
        // where the process has one thread and may call only what POSIX
        // lists as async-signal-safe. `become_session_leader` calls
        // `setsid`, `signal`, `prctl`, `getppid`, `getpid` and `write`,
        // which are on that list (`prctl` is a bare system call), and
        // builds its error from `errno` without allocating. The closure
        // itself loads one atomic integer. It touches no lock and no other
        // memory of the parent's. The guard's pipe is open in the child,
        // which is a copy of this process, until `exec` closes it.
        unsafe {
            command.pre_exec(move || {
                let pipe = for_child.load(Ordering::Relaxed);
                become_session_leader(harness, (pipe != NO_PIPE).then_some(pipe))
            });
        }
        let mut second_try = false;
        let (child, pending) = loop {
            // The guard first: it is running, and waiting for this one
            // tree, before the command exists.
            let pending = match guard {
                Some(guard) => Some(guard.prepare().map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("the process guard could not be started: {error}"),
                    )
                })?),
                None => None,
            };
            guard_pipe.store(
                pending.as_ref().map_or(NO_PIPE, |pending| pending.pipe()),
                Ordering::Relaxed,
            );
            match command.spawn() {
                Ok(child) => break (child, pending),
                Err(error) => {
                    // A broken pipe is the child's announcement finding
                    // the guard gone: it died after it was made ready.
                    // The next round starts another. Once.
                    let guard_died = pending.is_some()
                        && error.kind() == io::ErrorKind::BrokenPipe
                        && !second_try;
                    if let Some(pending) = pending {
                        pending.failed(guard_died);
                    }
                    if !guard_died {
                        return Err(error);
                    }
                    second_try = true;
                }
            }
        };
        // The command holds the caller's ends of its pipes. Let go of them
        // now, so that the pipes close when the tree's last writer does.
        drop(command);
        let pid = libc::pid_t::try_from(child.id())
            .map_err(|_| io::Error::other("the process id does not fit"))?;
        let mut inner = Self {
            pid,
            state: Mutex::new(State {
                child,
                ended: false,
            }),
            exit: Mutex::new(None),
            started: std::time::Instant::now(),
            watch: None,
        };
        if let Some(pending) = pending {
            match pending.started(child_group(pid)) {
                Ok(watch) => inner.watch = Some(watch),
                // Dropping `inner` kills the command again.
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("the process guard could not be told of the command: {error}"),
                    ));
                }
            }
        }
        Ok(inner)
    }

    pub(super) fn limits(&self) -> io::Result<Limits> {
        Ok(Limits::NONE)
    }

    pub(super) fn id(&self) -> u32 {
        child_group(self.pid)
    }

    pub(super) fn interrupt(&self) -> io::Result<()> {
        self.signal(libc::SIGINT)
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        self.signal(libc::SIGTERM)
    }

    pub(super) fn kill(&self) -> io::Result<()> {
        let state = lock(&self.state);
        if state.ended {
            return Err(ended());
        }
        signal_group(self.pid, libc::SIGKILL)?;
        // A process that made a process group of its own is still in the
        // command's session, and dies with it. The lock is held, so the
        // command is not reaped and the session's id is still its own.
        kill_session(self.pid);
        Ok(())
    }

    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        let state = lock(&self.state);
        if state.ended {
            return Err(ended());
        }
        signal_group(self.pid, signal)
    }

    pub(super) fn wait(&self) -> io::Result<Exit> {
        let mut exit = lock(&self.exit);
        if let Some(exit) = *exit {
            return Ok(exit);
        }
        // `end` takes this lock before it reaps, so the id is the command's
        // for as long as this call is waiting on it.
        let found = wait_without_reaping(self.pid)?;
        *exit = Some(found);
        Ok(found)
    }

    pub(super) fn others_running(&self) -> io::Result<usize> {
        let state = lock(&self.state);
        if state.ended {
            return Err(ended());
        }
        // The lock is held, so the command is not reaped and the group's
        // id is still its own.
        count_group(self.pid, self.started.elapsed())
    }

    pub(super) fn end(&self) {
        {
            let state = lock(&self.state);
            if state.ended {
                return;
            }
            // The command is not reaped yet, so the group and the session
            // are still ours.
            let _ = signal_group(self.pid, libc::SIGKILL);
            kill_session(self.pid);
        }
        // A thread in `wait` returns now that the command is dead, and
        // gives up the lock. If nobody asked, the exit is read here, while
        // it can still be read.
        let mut exit = lock(&self.exit);
        if exit.is_none() {
            *exit = Some(wait_without_reaping(self.pid).unwrap_or(Exit::Signal(libc::SIGKILL)));
        }
        let mut state = lock(&self.state);
        if state.ended {
            return;
        }
        // The guard forgets the group before its id can be anyone else's.
        if let Some(watch) = &self.watch {
            watch.release();
        }
        let _ = state.child.wait();
        state.ended = true;
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.end();
    }
}

/// A process id as the rest of the crate carries it.
fn child_group(pid: libc::pid_t) -> u32 {
    // A process id is positive.
    pid.unsigned_abs()
}

/// What the child does before it becomes the command.
fn become_session_leader(
    harness: libc::pid_t,
    guard_pipe: Option<std::os::fd::RawFd>,
) -> io::Result<()> {
    // SAFETY: `setsid` takes no argument and changes only this process:
    // it becomes the leader of a new session and a new process group, with
    // no controlling terminal. It cannot fail here for the documented
    // reason (the caller already leads a group), because the child was
    // forked a moment ago.
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    // The two signals the harness stops a command with mean what they say,
    // whatever the harness itself was started with: a parent that ignores
    // one would otherwise hand that down.
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: `signal` with `SIG_DFL` installs no handler; it resets
        // the disposition of one signal of this process.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
        }
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl(PR_SET_PDEATHSIG, signal)` takes one integer and
        // sets a flag on this process: the kernel sends it the signal when
        // the thread that forked it ends.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // The harness may have died between the fork and the line above,
        // and then nothing will ever send the signal.
        // SAFETY: `getppid` takes no argument and cannot fail.
        if unsafe { libc::getppid() } != harness {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = harness;
    // Last, when nothing of the above can fail any more: the guard is told
    // that this group is about to be a command.
    match guard_pipe {
        Some(pipe) => announce_to_guard(pipe),
        None => Ok(()),
    }
}

/// Writes `?<process id>` and a line break to the guard's pipe, from the
/// child, without allocating.
fn announce_to_guard(pipe: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: `getpid` takes no argument and cannot fail.
    let mut id = unsafe { libc::getpid() }.unsigned_abs();
    // The line, built from its end: a line break, the digits, the mark.
    let mut line = [0u8; 16];
    let mut at = line.len() - 1;
    line[at] = b'\n';
    loop {
        at -= 1;
        line[at] = b'0' + (id % 10) as u8;
        id /= 10;
        if id == 0 {
            break;
        }
    }
    at -= 1;
    line[at] = b'?';
    let line = &line[at..];
    // A guard that has died would make the write raise `SIGPIPE`, which
    // kills this process without a word. Ignored for the length of the
    // write, the failure comes back as an error instead, and the command
    // is not started.
    // SAFETY: `signal` with `SIG_IGN` or `SIG_DFL` installs no handler.
    // `write` is given a pointer to `line` and its length, which is valid
    // for the length of the call, and a descriptor the harness holds open
    // until the start is settled.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        let written = libc::write(pipe, line.as_ptr().cast(), line.len());
        // Read before anything else can change it.
        let result = match usize::try_from(written) {
            Ok(count) if count == line.len() => Ok(()),
            Ok(_) => Err(io::ErrorKind::WriteZero.into()),
            Err(_) => Err(io::Error::last_os_error()),
        };
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        result
    }
}

/// Sends `signal` to every process of the group `group` leads. A group
/// with nobody left in it is not an error.
///
/// The caller holds that `group` is a group this process started and has
/// not reaped the leader of.
pub(super) fn signal_group(group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    if group <= 1 {
        // `kill(-1, …)` is every process the user may signal, and `kill(0,
        // …)` is the caller's own group.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a process group of a command",
        ));
    }
    // SAFETY: `kill` takes two integers. A negative first argument names a
    // process group, and `group` is greater than 1, so this is never the
    // caller's own group nor "everyone".
    if unsafe { libc::kill(-group, signal) } == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

/// Kills every process of the session `leader` leads that is not in its
/// process group: one that made a group of its own, which a signal to the
/// group does not reach. In the sandbox no process can leave the session
/// (the filter answers `setsid` as done without doing it), so this reaches
/// all of a command's processes there.
///
/// Each process is held by a process descriptor before it is looked at
/// again and signalled, so a signal never reaches a process that took the
/// id of one that had ended. A kernel without process descriptors (before
/// Linux 5.3) is sent the signal by id after the second look.
///
/// The caller holds that `leader` leads a session this process started and
/// has not reaped.
#[cfg(target_os = "linux")]
#[allow(
    clippy::disallowed_methods,
    reason = "a read of the process table, as `count_group`'s"
)]
pub(super) fn kill_session(leader: libc::pid_t) {
    if leader <= 1 {
        return;
    }
    let session = leader.to_string();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        if id == leader || !in_session(id, &session) {
            continue;
        }
        // SAFETY: `pidfd_open` takes two integers and returns a new
        // descriptor or -1; the descriptor is owned at once.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, id, 0) };
        if fd < 0 {
            // No process descriptors on this kernel: the second look and
            // the signal by id.
            if in_session(id, &session) {
                // SAFETY: `kill` takes two integers; `id` is greater than
                // one and a process, not a group.
                unsafe { libc::kill(id, libc::SIGKILL) };
            }
            continue;
        }
        let fd = i32::try_from(fd).unwrap_or(-1);
        // SAFETY: `fd` was just returned by `pidfd_open` and nothing else
        // owns it.
        let held = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        // Looked at again while held: if the id is another process's now,
        // its session is not this one.
        if in_session(id, &session) {
            // SAFETY: the call takes a descriptor this function owns, a
            // signal number, a null pointer (no extra information) and no
            // flags.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    std::os::fd::AsRawFd::as_raw_fd(&held),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0u32,
                )
            };
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn kill_session(_leader: libc::pid_t) {}

/// Whether the live process `id` is in the session `session` and outside
/// its leader's group (the group is signalled as a whole).
#[cfg(target_os = "linux")]
#[allow(
    clippy::disallowed_methods,
    reason = "a read of the process table, as `count_group`'s"
)]
fn in_session(id: libc::pid_t, session: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{id}/stat")) else {
        return false;
    };
    let Some((_, fields)) = stat.rsplit_once(") ") else {
        return false;
    };
    let mut fields = fields.split(' ');
    let (Some(state), Some(_parent), Some(group), Some(its_session)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    its_session == session && group != session && state != "Z" && state != "X"
}

/// How many live processes are in the group `group` leads, or in the
/// session it leads, not counting the leader. A process that has exited
/// and waits to be reaped is not alive.
///
/// The process table has no list of a group's members, so the table is
/// read: a line for each process. Most of it need not be. Everything in a
/// command's group was started after the command, and the system gives
/// out process ids in rising order, so the members have ids above the
/// leader's, unless the ids wrapped round to the bottom meanwhile. For a
/// command that has run for less than a second with a million ids above
/// its own still unused, they cannot have: then only the higher ids are
/// read. Otherwise every process is.
#[cfg(target_os = "linux")]
fn count_group(group: libc::pid_t, age: std::time::Duration) -> io::Result<usize> {
    let leader_id = group.unsigned_abs();
    let above_only = age < std::time::Duration::from_secs(1)
        && highest_process_id()
            .is_some_and(|highest| highest.saturating_sub(leader_id) > IDS_NO_SECOND_USES_UP);
    let leader = group.to_string();
    let mut count = 0;
    for entry in std::fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        if id == leader_id || (above_only && id < leader_id) {
            continue;
        }
        // A process may end between the listing and the read.
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (name) state parent group …`. The name may hold anything,
        // brackets and spaces included, so the fields are counted from the
        // last closing bracket.
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            continue;
        };
        let mut fields = fields.split(' ');
        let (Some(state), Some(_parent), Some(its_group), Some(its_session)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if (its_group == leader || its_session == leader) && state != "Z" && state != "X" {
            count += 1;
        }
    }
    Ok(count)
}

/// More process ids than a machine gives out in a second.
#[cfg(target_os = "linux")]
const IDS_NO_SECOND_USES_UP: u32 = 1_000_000;

/// The highest process id the system gives out before it starts again
/// from the bottom. Read once: it is a setting of the system.
#[cfg(target_os = "linux")]
fn highest_process_id() -> Option<u32> {
    static HIGHEST: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *HIGHEST.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/pid_max")
            .ok()?
            .trim()
            .parse()
            .ok()
    })
}

#[cfg(not(target_os = "linux"))]
fn count_group(_group: libc::pid_t, _age: std::time::Duration) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this system has no process table to read",
    ))
}

/// Blocks until the child `pid` has exited, and leaves it unreaped: its
/// process id, and so its group's id, stays taken.
fn wait_without_reaping(pid: libc::pid_t) -> io::Result<Exit> {
    loop {
        // SAFETY: `siginfo_t` is a C structure of integers and pointers,
        // for which all zeroes is a valid value.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, writable `siginfo_t` for the length of
        // the call, and `pid` is a positive process id.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid.unsigned_abs(),
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // SAFETY: `waitid` succeeded for `WEXITED`, so it filled the fields
        // of the structure that describe a child, which `si_status` reads.
        let status = unsafe { info.si_status() };
        return Ok(if info.si_code == libc::CLD_EXITED {
            Exit::Code(status)
        } else {
            Exit::Signal(status)
        });
    }
}

/// Makes sure a child that has exited stays until it is waited for.
///
/// A process that ignores `SIGCHLD` has its children reaped by the system
/// the moment they exit. Their process ids are then free at once, and
/// nothing here could tell a command's group from a stranger's that was
/// given the same id. A harness is not started that way by a terminal,
/// and can be by a service manager or a careless parent, which hand the
/// setting down. It is put back to the default, which every program that
/// waits for a child needs anyway: the standard library's own `wait`
/// fails without it.
fn keep_exited_children() {
    // SAFETY: `sigaction` with a null new action only reads the current
    // one into `current`, a zeroed and writable `sigaction` structure,
    // for which all zeroes is a valid value.
    let ignored = unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut current) == 0
            && (current.sa_sigaction == libc::SIG_IGN || current.sa_flags & libc::SA_NOCLDWAIT != 0)
    };
    if ignored {
        // SAFETY: `signal` with `SIG_DFL` installs no handler; it resets
        // the disposition of one signal, and with it the flag that asks
        // for children not to be kept.
        unsafe {
            libc::signal(libc::SIGCHLD, libc::SIG_DFL);
        }
    }
}

/// Gives the calling process a name of its own in the process table, so
/// that a command that kills the harness by its name does not take the
/// guard with it: the guard has work to do when the harness is gone.
///
/// The name of a process is the name of its first thread, which need not
/// be the thread that calls this, so it is set through the process table
/// itself. Where that cannot be done the guard keeps the harness's name.
pub(super) fn name_the_guard() {
    #[cfg(target_os = "linux")]
    {
        let first_thread = format!("/proc/self/task/{}/comm", std::process::id());
        let _ = std::fs::write(first_thread, "cx-tree-guard");
    }
}

/// Makes the calling process ignore the signals that end a harness: the
/// guard must outlive them to do its work.
pub(super) fn ignore_stop_signals() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
        // SAFETY: `signal` with `SIG_IGN` installs no handler; it sets the
        // disposition of one signal of this process.
        unsafe {
            libc::signal(signal, libc::SIG_IGN);
        }
    }
}

/// The process group of the calling process.
pub(super) fn own_group() -> libc::pid_t {
    // SAFETY: `getpgrp` takes no argument and cannot fail.
    unsafe { libc::getpgrp() }
}

/// Makes `command` start as the leader of a session of its own, with none
/// of the rest of what a tree is. For the guard.
pub(super) fn in_own_session(command: &mut Command) {
    // SAFETY: as in `Inner::spawn`: the function runs between `fork` and
    // `exec` and calls `setsid` alone, which is async-signal-safe.
    unsafe {
        command.pre_exec(start_session);
    }
}

fn start_session() -> io::Result<()> {
    // SAFETY: `setsid` takes no argument and changes only this process.
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The reading end of a command's output, and the pipe that stops a read.
#[derive(Debug)]
pub(super) struct OutputPipe {
    pipe: PipeReader,
    stop: PipeReader,
}

/// The other end of the pipe that stops a read. Closing it is the signal.
#[derive(Debug, Clone)]
pub(super) struct StopHandle {
    writer: Arc<Mutex<Option<PipeWriter>>>,
}

impl OutputPipe {
    pub(super) fn new(pipe: PipeReader) -> io::Result<(Self, StopHandle)> {
        let (stop, writer) = io::pipe()?;
        Ok((
            Self { pipe, stop },
            StopHandle {
                writer: Arc::new(Mutex::new(Some(writer))),
            },
        ))
    }

    pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<ReadOutcome> {
        loop {
            let mut waits = [
                libc::pollfd {
                    fd: self.pipe.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.stop.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: `waits` is an array of two valid `pollfd` entries
            // that lives for the length of the call, and both descriptors
            // are open: `self` owns them.
            let ready = unsafe { libc::poll(waits.as_mut_ptr(), 2, -1) };
            if ready == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            // The stop pipe is never written: anything on it is its other
            // end having closed.
            if waits[1].revents != 0 {
                return Ok(ReadOutcome::Stopped);
            }
            if waits[0].revents != 0 {
                // There are bytes, or every writer is gone and this
                // returns 0 at once.
                return Ok(match self.pipe.read(buffer) {
                    Ok(0) => ReadOutcome::Closed,
                    Ok(bytes) => ReadOutcome::Bytes(bytes),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                });
            }
        }
    }
}

impl StopHandle {
    pub(super) fn stop(&self) {
        lock(&self.writer).take();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use super::*;

    /// The count reads only part of the process table for a young tree,
    /// and all of it for an old one. Both must find the same members.
    #[test]
    fn sys_a_group_is_counted_the_same_whichever_way_the_table_is_read() {
        // A shell and the two ends of a pipeline, which stay for as long as
        // the first one's input is open: the command itself, and two
        // processes it started.
        let mut command = Command::new("sh");
        command
            .args(["-c", "cat | cat"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (reader, input) = io::pipe().expect("make a pipe");
        command.stdin(reader);
        let tree = Inner::spawn(command, None, Limits::NONE).expect("start the command");

        let young = Duration::ZERO;
        let old = Duration::from_secs(60);
        let begun = Instant::now();
        while count_group(tree.pid, old).expect("read the table") != 2 {
            assert!(
                begun.elapsed() < Duration::from_secs(30),
                "the two processes never appeared"
            );
            std::thread::yield_now();
        }
        assert_eq!(count_group(tree.pid, young).expect("read the table"), 2);
        assert_eq!(count_group(tree.pid, old).expect("read the table"), 2);
        // A group that is not there has nobody in it, either way.
        assert_eq!(count_group(libc::pid_t::MAX - 1, young).expect("read"), 0);
        assert_eq!(count_group(libc::pid_t::MAX - 1, old).expect("read"), 0);

        // Their input closes, and both go.
        drop(input);
        assert_eq!(tree.wait().expect("wait"), Exit::Code(0));
        let begun = Instant::now();
        while count_group(tree.pid, old).expect("read the table") != 0 {
            assert!(begun.elapsed() < Duration::from_secs(30), "it never left");
            std::thread::yield_now();
        }
        assert_eq!(count_group(tree.pid, young).expect("read the table"), 0);
        tree.end();
    }
}
