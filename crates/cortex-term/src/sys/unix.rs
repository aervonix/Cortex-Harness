//! The Unix half of the terminal layer: raw mode, the window's size, waiting
//! for input, and putting the terminal back on every way out.
//!
//! The standard library can tell whether a stream is a terminal, and no
//! more. Raw mode (`termios`), the window size, `poll`, and signal handlers
//! go through `libc` here, and every `unsafe` block says why it is sound.
//!
//! # Signals
//!
//! A signal handler may only call functions that are safe in one. The ones
//! here use `tcsetattr`, `write`, `signal`, `sigprocmask`, `raise` and atomic
//! loads and stores, all of which POSIX lists as async-signal-safe, and read
//! state that was put in place before the handlers were installed and is
//! never moved after.

#![allow(
    unsafe_code,
    reason = "raw mode, the window size and signal handlers are only reachable through the \
              C library; cortex-term is where such calls live"
)]

use std::io;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use super::{Ready, restore_sequence};

const STDIN: libc::c_int = 0;
const STDOUT: libc::c_int = 1;

/// The terminal's settings as they were before raw mode was first turned
/// on, which every way out puts back. Set once, before any handler that
/// reads it is installed.
static ORIGINAL: OnceLock<libc::termios> = OnceLock::new();
/// The raw settings, for putting raw mode back after a suspension.
static RAW: OnceLock<libc::termios> = OnceLock::new();
/// Whether raw mode is on now.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The writing end of the pipe that wakes a wait: written by a waker and by
/// the handlers of SIGWINCH and SIGCONT. -1 until it exists.
static WAKE_WRITE: AtomicI32 = AtomicI32::new(-1);
/// The reading end of the same pipe.
static WAKE_READ: AtomicI32 = AtomicI32::new(-1);
/// Set by the SIGWINCH handler.
static RESIZED: AtomicBool = AtomicBool::new(false);
/// Set when the program was stopped and has been continued.
static RESUMED: AtomicBool = AtomicBool::new(false);
/// Set by a waker.
static WOKEN: AtomicBool = AtomicBool::new(false);
/// The handlers that were in place before ours, put back on leaving.
static PREVIOUS: Mutex<Vec<(libc::c_int, libc::sigaction)>> = Mutex::new(Vec::new());

/// The signals that end the program, after which the terminal is put back.
const ENDING: [libc::c_int; 4] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT];

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Whether standard input and standard output are both a terminal.
pub(crate) fn is_terminal() -> bool {
    // SAFETY: `isatty` reads nothing but its argument.
    unsafe { libc::isatty(STDIN) == 1 && libc::isatty(STDOUT) == 1 }
}

/// Turns raw mode on and installs the handlers that put the terminal back.
pub(crate) fn enter() -> io::Result<()> {
    if !is_terminal() {
        return Err(io::Error::other(
            "standard input and standard output must both be a terminal",
        ));
    }
    let mut current = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `tcgetattr` fills the structure it is given, which is large
    // enough because it has the type the function asks for.
    check(unsafe { libc::tcgetattr(STDIN, current.as_mut_ptr()) })?;
    // SAFETY: `tcgetattr` succeeded, so the structure is filled.
    let current = unsafe { current.assume_init() };
    let original = *ORIGINAL.get_or_init(|| current);
    let mut raw = original;
    // SAFETY: `cfmakeraw` changes the flags of the structure it is given.
    unsafe { libc::cfmakeraw(&raw mut raw) };
    let raw = *RAW.get_or_init(|| raw);
    wake_pipe()?;
    install_handlers()?;
    // SAFETY: `raw` is a complete `termios` made from the terminal's own.
    check(unsafe { libc::tcsetattr(STDIN, libc::TCSADRAIN, &raw const raw) })?;
    ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Puts the terminal's settings back, and the handlers that were there
/// before. Safe to call more than once.
pub(crate) fn leave() {
    restore_modes();
    let mut previous = PREVIOUS.lock().unwrap_or_else(PoisonError::into_inner);
    for (signal, action) in previous.drain(..) {
        // SAFETY: `action` is what `sigaction` gave back for this signal.
        unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) };
    }
}

/// Writes the restore sequence and puts the original settings back, if
/// raw mode is on. Async-signal-safe: see the module documentation.
pub(crate) fn emergency() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let bytes = restore_sequence();
    // SAFETY: the pointer and length are those of a static byte string.
    unsafe { libc::write(STDOUT, bytes.as_ptr().cast(), bytes.len()) };
    if let Some(original) = ORIGINAL.get() {
        // SAFETY: `original` is the complete `termios` the terminal had.
        unsafe { libc::tcsetattr(STDIN, libc::TCSADRAIN, original) };
    }
}

/// Puts the original settings back without writing anything: for a normal
/// exit, where the presenter has already finished.
///
/// Raw mode is marked off only once the settings are back: a signal that
/// comes in between still finds it on, and puts the terminal back itself.
fn restore_modes() {
    if ACTIVE.load(Ordering::SeqCst)
        && let Some(original) = ORIGINAL.get()
    {
        // SAFETY: `original` is the complete `termios` the terminal had.
        unsafe { libc::tcsetattr(STDIN, libc::TCSADRAIN, original) };
    }
    ACTIVE.store(false, Ordering::SeqCst);
}

/// Makes the pipe that wakes a wait, once.
fn wake_pipe() -> io::Result<()> {
    if WAKE_READ.load(Ordering::SeqCst) >= 0 {
        return Ok(());
    }
    let mut fds = [0; 2];
    // SAFETY: `pipe` fills an array of two descriptors.
    check(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
    for fd in fds {
        // SAFETY: both descriptors were just made; the flags only change how
        // they behave.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    WAKE_READ.store(fds[0], Ordering::SeqCst);
    WAKE_WRITE.store(fds[1], Ordering::SeqCst);
    Ok(())
}

/// Writes one byte to the wake pipe. Async-signal-safe.
fn poke() {
    let fd = WAKE_WRITE.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1u8;
        // SAFETY: one byte from a local; a full pipe refuses it, which is
        // fine: a wake is already waiting.
        unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
    }
}

/// Wakes [`wait`] from another thread.
pub(crate) fn wake() {
    WOKEN.store(true, Ordering::SeqCst);
    poke();
}

extern "C" fn on_resize(_: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
    poke();
}

extern "C" fn on_ending(signal: libc::c_int) {
    emergency();
    // SAFETY: putting the default action back and raising the signal again
    // ends the process as the signal would have.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

extern "C" fn on_stop(_: libc::c_int) {
    let was_active = ACTIVE.load(Ordering::SeqCst);
    emergency();
    // SAFETY: stop with the default action, which needs SIGTSTP unblocked
    // inside its own handler; when the program is continued the stop
    // returns, and the handler and raw mode are put back.
    unsafe {
        libc::signal(libc::SIGTSTP, libc::SIG_DFL);
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        libc::sigemptyset(set.as_mut_ptr());
        libc::sigaddset(set.as_mut_ptr(), libc::SIGTSTP);
        libc::sigprocmask(libc::SIG_UNBLOCK, set.as_ptr(), std::ptr::null_mut());
        libc::raise(libc::SIGTSTP);
        // Continued.
        let handler: extern "C" fn(libc::c_int) = on_stop;
        libc::signal(libc::SIGTSTP, handler as libc::sighandler_t);
        if was_active && let Some(raw) = RAW.get() {
            libc::tcsetattr(STDIN, libc::TCSADRAIN, raw);
            ACTIVE.store(true, Ordering::SeqCst);
        }
    }
    RESUMED.store(true, Ordering::SeqCst);
    poke();
}

fn install(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) -> io::Result<()> {
    let mut action = MaybeUninit::<libc::sigaction>::zeroed();
    let mut old = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a zeroed `sigaction` is valid; the handler and the flags are
    // set before it is used, and `old` is filled by the call.
    unsafe {
        let pointer = action.as_mut_ptr();
        (*pointer).sa_sigaction = handler as libc::sighandler_t;
        (*pointer).sa_flags = libc::SA_RESTART;
        // While one handler runs, every other signal it shares the restore
        // with waits: a second signal cannot find the terminal half put
        // back and end the process before the first has finished.
        let mask = &raw mut (*pointer).sa_mask;
        libc::sigemptyset(mask);
        for other in ENDING.into_iter().chain([libc::SIGTSTP, libc::SIGWINCH]) {
            libc::sigaddset(mask, other);
        }
        check(libc::sigaction(signal, pointer, old.as_mut_ptr()))?;
        PREVIOUS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((signal, old.assume_init()));
    }
    Ok(())
}

fn install_handlers() -> io::Result<()> {
    if !PREVIOUS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_empty()
    {
        return Ok(());
    }
    install(libc::SIGWINCH, on_resize)?;
    install(libc::SIGTSTP, on_stop)?;
    for signal in ENDING {
        install(signal, on_ending)?;
    }
    Ok(())
}

/// Stops the program as Ctrl+Z would, with the terminal put back while it
/// is stopped and raw mode put back when it is continued.
pub(crate) fn suspend() {
    // SAFETY: SIGTSTP goes to the handler above, or, if none is installed,
    // stops the process as it would anyway.
    unsafe { libc::raise(libc::SIGTSTP) };
}

/// The window's size, columns then rows.
pub(crate) fn size() -> io::Result<(u16, u16)> {
    let mut size = MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: `TIOCGWINSZ` fills a `winsize`.
    check(unsafe { libc::ioctl(STDOUT, libc::TIOCGWINSZ, size.as_mut_ptr()) })?;
    // SAFETY: zeroed, then filled by the call.
    let size = unsafe { size.assume_init() };
    Ok((size.ws_col, size.ws_row))
}

/// Writes all of `bytes` to standard output: one call, unless the terminal
/// takes only part of it.
///
/// Once the terminal has been put back (a panic on another thread, a
/// signal), nothing more is written: a frame would turn modes on again in a
/// terminal that is no longer the program's.
pub(crate) fn write(bytes: &[u8]) -> io::Result<()> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(io::Error::other("the terminal has been put back"));
    }
    let mut rest = bytes;
    while !rest.is_empty() {
        // SAFETY: the pointer and length are those of a live slice.
        let written = unsafe { libc::write(STDOUT, rest.as_ptr().cast(), rest.len()) };
        match usize::try_from(written) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => rest = &rest[count..],
            Err(_) => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// Waits for input, a resize, a resumption or a wake, for at most
/// `timeout` (`None`: as long as it takes). Input is added to `into`.
pub(crate) fn wait(timeout: Option<Duration>, into: &mut Vec<u8>) -> io::Result<Ready> {
    let millis = timeout.map_or(-1, |timeout| {
        let rounded_up = timeout.as_nanos().div_ceil(1_000_000);
        libc::c_int::try_from(rounded_up).unwrap_or(libc::c_int::MAX)
    });
    let wake = WAKE_READ.load(Ordering::SeqCst);
    loop {
        if let Some(ready) = flags() {
            return Ok(ready);
        }
        let mut fds = [
            libc::pollfd {
                fd: STDIN,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: an array of two `pollfd`, and its length.
        let count = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) };
        if count == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(flags().unwrap_or(Ready::Timeout));
        }
        if fds[1].revents != 0 {
            drain(wake);
        }
        if fds[0].revents != 0 {
            let mut buffer = [0u8; 4096];
            // SAFETY: reads into a local buffer of its own length.
            let read = unsafe { libc::read(STDIN, buffer.as_mut_ptr().cast(), buffer.len()) };
            match usize::try_from(read) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => {
                    into.extend_from_slice(&buffer[..count]);
                    return Ok(Ready::Input);
                }
                Err(_) => {
                    let error = io::Error::last_os_error();
                    if !matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) {
                        return Err(error);
                    }
                }
            }
        }
    }
}

fn flags() -> Option<Ready> {
    if RESUMED.swap(false, Ordering::SeqCst) {
        RESIZED.store(false, Ordering::SeqCst);
        return Some(Ready::Resumed);
    }
    if RESIZED.swap(false, Ordering::SeqCst) {
        return Some(Ready::Resize);
    }
    if WOKEN.swap(false, Ordering::SeqCst) {
        return Some(Ready::Woken);
    }
    None
}

fn drain(fd: libc::c_int) {
    let mut buffer = [0u8; 64];
    // SAFETY: reads into a local buffer of its own length from a
    // non-blocking pipe, until it is empty.
    while unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
}

/// A description of the terminal's settings on standard input, for tests
/// that check they were put back: every field of its `termios`.
#[cfg(feature = "pty")]
pub(crate) fn modes() -> io::Result<String> {
    let mut current = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: as in `enter`.
    check(unsafe { libc::tcgetattr(STDIN, current.as_mut_ptr()) })?;
    // SAFETY: filled by the call.
    let t = unsafe { current.assume_init() };
    Ok(format!(
        "iflag={:x} oflag={:x} cflag={:x} lflag={:x} cc={:?}",
        t.c_iflag, t.c_oflag, t.c_cflag, t.c_lflag, t.c_cc
    ))
}

/// Pseudo-terminals, for tests.
#[cfg(feature = "pty")]
pub(crate) mod pty {
    #![allow(
        clippy::disallowed_types,
        reason = "test support: it starts a process in a pseudo-terminal, and reads and writes \
                  the terminal's other end as a file"
    )]

    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, ExitStatus, Stdio};

    use super::check;

    /// A program running in a pseudo-terminal of its own.
    #[derive(Debug)]
    pub(crate) struct Pty {
        master: OwnedFd,
        child: Child,
    }

    fn set_size(fd: &OwnedFd, cols: u16, rows: u16) -> io::Result<()> {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `TIOCSWINSZ` reads a `winsize`.
        check(unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) })?;
        Ok(())
    }

    impl Pty {
        pub(crate) fn spawn(
            program: &str,
            arguments: &[String],
            env: &[(String, String)],
            cols: u16,
            rows: u16,
        ) -> io::Result<Self> {
            // SAFETY: `posix_openpt` returns a new descriptor or -1.
            let master = check(unsafe {
                libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC)
            })?;
            // SAFETY: the descriptor was just made, and is ours alone.
            let master = unsafe { OwnedFd::from_raw_fd(master) };
            // SAFETY: both act on the master just made.
            unsafe {
                check(libc::grantpt(master.as_raw_fd()))?;
                check(libc::unlockpt(master.as_raw_fd()))?;
                check(libc::fcntl(
                    master.as_raw_fd(),
                    libc::F_SETFD,
                    libc::FD_CLOEXEC,
                ))?;
            }
            let mut name = [0 as libc::c_char; 128];
            // SAFETY: the buffer and its length.
            let failed =
                unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
            if failed != 0 {
                return Err(io::Error::from_raw_os_error(failed));
            }
            // SAFETY: `ptsname_r` wrote a string ending in a zero.
            let slave = check(unsafe {
                libc::open(
                    name.as_ptr(),
                    libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
                )
            })?;
            // SAFETY: the descriptor was just opened, and is ours alone.
            let slave = unsafe { OwnedFd::from_raw_fd(slave) };
            set_size(&master, cols, rows)?;

            let mut command = Command::new(program);
            command
                .args(arguments)
                .env("TERM", "xterm-256color")
                .envs(env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // SAFETY: between fork and exec only `setsid` and `ioctl` run,
            // both async-signal-safe: a session of its own, with the
            // pseudo-terminal (its standard input by now) as its controlling
            // terminal.
            unsafe {
                command.pre_exec(|| {
                    check(libc::setsid())?;
                    check(libc::ioctl(0, libc::TIOCSCTTY, 0))?;
                    Ok(())
                });
            }
            let child = command.spawn()?;
            Ok(Self { master, child })
        }

        pub(crate) fn reader(&self) -> io::Result<File> {
            Ok(File::from(self.master.try_clone()?))
        }

        pub(crate) fn writer(&self) -> io::Result<File> {
            Ok(File::from(self.master.try_clone()?))
        }

        pub(crate) fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
            set_size(&self.master, cols, rows)
        }

        pub(crate) fn pid(&self) -> u32 {
            self.child.id()
        }

        pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.child.try_wait()
        }

        pub(crate) fn kill(&mut self) -> io::Result<()> {
            self.child.kill()
        }
    }

    /// Whether a read error on the master means the other side has gone:
    /// Linux reports that as EIO once every program in the terminal has
    /// closed it.
    pub(crate) fn is_end(error: &io::Error) -> bool {
        error.raw_os_error() == Some(libc::EIO)
    }

    /// Sends `signal` to process `pid`.
    pub(crate) fn signal(pid: u32, signal: i32) -> io::Result<()> {
        let pid = libc::pid_t::try_from(pid).map_err(io::Error::other)?;
        // SAFETY: `kill` reads nothing but its arguments.
        check(unsafe { libc::kill(pid, signal) })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_written_while_the_terminal_is_not_the_programs() {
        // The test process never entered raw mode: as after a restore.
        assert!(!ACTIVE.load(Ordering::SeqCst));
        assert!(write(b"\x1b[?25l").is_err());
    }
}
