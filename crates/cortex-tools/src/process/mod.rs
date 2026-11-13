//! The process runner: one command, from its start to the end of its tree
//! (`docs/architecture/05-tools.md`, "`shell`").
//!
//! [`Runner::run`] starts a command as a process tree
//! ([`cortex_sandbox::tree`]), collects what it writes, and does not return
//! until nothing of the tree is left.
//!
//! - **One pipe.** Standard output and standard error are the same pipe,
//!   so their lines arrive in the order they were written.
//! - **A bounded buffer.** The beginning and the end of the output are
//!   kept, and once it passes the buffer all of it also goes to an
//!   artifact ([`Captured`]).
//! - **Stopping is staged.** On the time limit or on cancellation the tree
//!   is asked politely, then told, then killed, with a grace between the
//!   stages ([`Stop`]).
//! - **Nothing is left behind.** When the command has exited, what it left
//!   running is given a short grace and then killed, and the result says
//!   so. That holds on every path, and when the future is dropped.
//!
//! # Threads
//!
//! Reading a pipe and waiting for a process both block, so each command
//! has two threads of its own for them, and the future that runs the
//! command only waits. They are plain threads, not the blocking pool's: a
//! long command would otherwise take two of the pool's few threads for as
//! long as it runs. Both end when the tree does.
//!
//! The command is started from the thread that polls the future, never
//! from a pool: on Linux the kernel's death signal is tied to the thread
//! that started the command ([`Tree::spawn`]).
//!
//! # Time
//!
//! The runner waits on a [`Timer`], and a test gives it one whose waits
//! end when the test says so. Nothing here sleeps.

#![allow(
    clippy::disallowed_types,
    reason = "this is the process runner that the rule sends every command through; \
              starting one takes well under a millisecond, and the waiting is on threads"
)]

pub(crate) mod brief;
mod capture;
mod decode;

use std::ffi::OsString;
use std::future::{Future, poll_fn};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use cortex_base::{AbsPath, CancelToken, SharedClock, SystemClock};
use cortex_sandbox::tree::{Output, OutputStop, ReadOutcome, Tree};

pub use capture::{Captured, KeptText};
pub use cortex_sandbox::tree::{Exit, Guard};

use crate::contract::ToolOutput;
use crate::store::ContentStore;
use capture::Capture;
use decode::Utf8Stream;

/// How long a stage of stopping is given before the next one.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(2);

/// How long what a command left running is given to finish by itself.
pub const DEFAULT_LINGER: Duration = Duration::from_millis(500);

/// The shortest time between two pieces of output sent to a front end:
/// one frame.
pub const DEFAULT_FRAME: Duration = Duration::from_millis(16);

/// How long the runner waits for a killed tree to be gone before it gives
/// up on what it cannot see. A kill takes milliseconds: this bounds a
/// failure of the system, and no test waits it out.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How long the runner waits for the output to end once nothing of the
/// tree is left to write it. It ends at once, unless a process that left
/// the tree holds the pipe: then the runner stops listening after this.
pub const SETTLE: Duration = Duration::from_secs(1);

/// How much text may wait to be sent to a front end. A front end that is
/// slower than the command misses output from the live view, never from
/// the result.
const PENDING_BYTES: usize = 64 * 1024;

/// A wait in progress. Dropping it gives the wait up.
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Something that can wait: the runner's only source of time passing.
pub trait Timer: Send + Sync + std::fmt::Debug {
    /// A future that completes after `duration`.
    fn sleep(&self, duration: Duration) -> Sleep;
}

/// The async runtime's timer: what the harness runs with. It must be used
/// inside a Tokio runtime that has its timer enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioTimer;

impl Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// A program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    /// The program: a path, or a name looked up on `PATH`.
    pub program: PathBuf,
    /// Its arguments, in order.
    pub arguments: Vec<Argument>,
}

/// One argument of a [`CommandLine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Argument {
    /// One argument, which the program receives exactly as it is here. On
    /// Windows it is quoted by the rules of the C runtime, which is how
    /// nearly every program reads its command line.
    Exact(String),
    /// Text put on a Windows command line as it is, for a program that
    /// reads its command line by rules of its own (`cmd.exe`). Elsewhere a
    /// process is given its arguments one by one, and this is one of them.
    Verbatim(String),
}

impl CommandLine {
    /// A command line with no arguments yet.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
        }
    }

    /// Adds an argument the program receives exactly.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.arguments.push(Argument::Exact(argument.into()));
        self
    }

    /// Adds several arguments the program receives exactly.
    #[must_use]
    pub fn args(mut self, arguments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.arguments
            .extend(arguments.into_iter().map(|one| Argument::Exact(one.into())));
        self
    }

    /// Adds text that a Windows command line carries as it is.
    #[must_use]
    pub fn verbatim(mut self, text: impl Into<String>) -> Self {
        self.arguments.push(Argument::Verbatim(text.into()));
        self
    }
}

/// What starts a command: the hook the sandbox fills
/// (`docs/architecture/06-permissions-and-sandbox.md`, "How it is
/// applied").
///
/// The shell tool builds the command line of the shell, and hands it to
/// its launcher before anything starts. [`Direct`] returns it as it is.
/// The sandbox returns the command line of the harness's own launcher,
/// with the session's policy for its standard input, which applies the
/// policy and then becomes the command.
pub trait Launcher: Send + Sync + std::fmt::Debug {
    /// What is started in place of `command`. An error is told to the
    /// model, and nothing is started: a command that cannot be confined as
    /// the policy says does not run unconfined.
    fn launch(&self, command: CommandLine, cx: &LaunchCx<'_>) -> Result<Launch, String>;

    /// One line for the model about what the launcher's confinement did to
    /// a command that has ended: which rule blocked it. `None` when there
    /// is nothing to say.
    fn explain(&self, exit: Option<Exit>, output: &str) -> Option<String> {
        let _ = (exit, output);
        None
    }
}

/// What a [`Launcher`] is told about the call.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LaunchCx<'a> {
    /// The workspace root.
    pub workspace: &'a AbsPath,
    /// The directory the command runs in.
    pub cwd: &'a AbsPath,
}

impl<'a> LaunchCx<'a> {
    /// What a launcher is told of a call in `workspace` that runs in `cwd`.
    #[must_use]
    pub fn new(workspace: &'a AbsPath, cwd: &'a AbsPath) -> Self {
        Self { workspace, cwd }
    }
}

/// What a [`Launcher`] wants started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The command line to start.
    pub command: CommandLine,
    /// Bytes for the standard input of what is started, which then sees
    /// the end of its input. `None` gives it no input at all. A launcher
    /// reads its policy from here and gives the command a closed input.
    pub stdin: Option<Vec<u8>>,
}

/// The launcher that confines nothing: the command line is started as it
/// is. What a session has until the sandbox step, and at the
/// `unrestricted` level after it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Direct;

impl Launcher for Direct {
    fn launch(&self, command: CommandLine, _cx: &LaunchCx<'_>) -> Result<Launch, String> {
        Ok(Launch {
            command,
            stdin: None,
        })
    }
}

/// One command to run.
#[derive(Debug, Clone)]
pub struct Start {
    /// What to start, as the launcher returned it.
    pub launch: Launch,
    /// The directory it runs in.
    pub cwd: PathBuf,
    /// Its whole environment. Nothing is inherited beyond this.
    pub env: Vec<(OsString, OsString)>,
    /// How long it may run before it is stopped.
    pub timeout: Duration,
    /// What an artifact of its output is called.
    pub label: String,
}

/// A stage of stopping a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stop {
    /// The polite interrupt: `SIGINT` to the group, or a Ctrl-Break event.
    Interrupt,
    /// `SIGTERM` to the group. Windows has no such stage.
    Terminate,
    /// `SIGKILL` to the group, or the end of the Job Object.
    Kill,
}

/// Why a command's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ending {
    /// The command exited by itself.
    Exited,
    /// It ran into its time limit and was stopped.
    TimedOut,
    /// The call was cancelled and the command was stopped.
    Interrupted,
}

/// A command that has ended, with nothing of its tree left.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// How the command itself ended. `None` when the system would not say.
    pub exit: Option<Exit>,
    /// Why the run ended.
    pub ending: Ending,
    /// The last stage of stopping that was needed, when the command was
    /// stopped.
    pub stopped_with: Option<Stop>,
    /// Whether the command exited and left processes running, which were
    /// then killed.
    pub left_running: bool,
    /// Whether something the command started was still holding its output
    /// after the tree was killed: a process that left the tree, which the
    /// harness cannot stop.
    pub outlived: bool,
    /// Whether what the command wrote could not be collected: the reader
    /// did not finish, or the content store failed under it. `output` is
    /// then empty, and that is not the command's doing.
    pub output_lost: bool,
    /// How long the run took, from the start of the command to the end of
    /// its tree.
    pub duration: Duration,
    /// What it wrote.
    pub output: Captured,
}

/// Why a command could not be started.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The program is not there, or may not be run.
    #[error("`{program}` could not be started: {source}")]
    Spawn {
        /// The program that was asked for.
        program: String,
        /// What the system said.
        source: std::io::Error,
    },
    /// The pipe or a thread for the command could not be made.
    #[error("the command could not be set up: {0}")]
    Setup(std::io::Error),
}

/// Runs commands. Cheap to clone: the parts are shared.
#[derive(Debug, Clone)]
pub struct Runner {
    timer: Arc<dyn Timer>,
    clock: SharedClock,
    guard: Option<Guard>,
    grace: Duration,
    linger: Duration,
    frame: Duration,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    /// A runner on the runtime's timer and the system clock, with no
    /// guard and the default graces.
    #[must_use]
    pub fn new() -> Self {
        Self {
            timer: Arc::new(TokioTimer),
            clock: Arc::new(SystemClock::new()),
            guard: None,
            grace: DEFAULT_GRACE,
            linger: DEFAULT_LINGER,
            frame: DEFAULT_FRAME,
        }
    }

    /// Sets what the runner waits on.
    #[must_use]
    pub fn with_timer(mut self, timer: Arc<dyn Timer>) -> Self {
        self.timer = timer;
        self
    }

    /// Sets the clock that durations are measured on.
    #[must_use]
    pub fn with_clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// Sets the guard that kills the trees of a harness that died
    /// ([`Guard`]). Without one, Linux kills only the command itself then.
    #[must_use]
    pub fn with_guard(mut self, guard: Guard) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Sets the grace between two stages of stopping.
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Sets how long what a command left running is given to finish.
    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Sets the shortest time between two pieces of streamed output. Zero
    /// sends each piece as it arrives.
    #[must_use]
    pub fn with_frame(mut self, frame: Duration) -> Self {
        self.frame = frame;
        self
    }

    /// Runs one command to the end of its tree.
    ///
    /// - What it writes goes to `out` as it arrives, at most once a frame,
    ///   and into the result's [`Captured`]. Once the output passes the
    ///   buffer, all of it is written to an artifact in `store`.
    /// - When `start.timeout` passes or `cancel` fires, the tree is
    ///   stopped in stages.
    /// - When the command exits, what it left running has
    ///   [`DEFAULT_LINGER`] to finish, and is then killed.
    /// - If the future is dropped, the tree is killed.
    ///
    /// It must be polled on a thread that outlives the command: see the
    /// [module](self) documentation.
    pub async fn run(
        &self,
        start: Start,
        cancel: &CancelToken,
        store: Arc<dyn ContentStore>,
        out: &mut dyn ToolOutput,
    ) -> Result<Finished, StartError> {
        let started = self.clock.monotonic();
        let shared = Arc::new(Shared::default());
        let running = self.start(&start, store, &shared)?;
        let mut waits = Waits {
            shared: &shared,
            cancelled: Box::pin(cancel.cancelled()),
            timeout: Some(self.timer.sleep(start.timeout)),
            deadline: None,
            frame: None,
        };
        let mut run = Run {
            runner: self,
            tree: &running.tree,
            phase: Phase::Running,
            ending: Ending::Exited,
            stop: &running.stop,
            stopped_with: None,
            left_running: false,
            outlived: false,
            nothing_left: false,
            exit: None,
            reader_done: false,
        };

        loop {
            let mut ready = poll_fn(|cx| waits.poll(cx)).await;
            // Output first, so that a front end sees a command's last words
            // before it sees that the command has ended.
            if let Some(text) = ready.text.take() {
                out.output(&text);
                if !self.frame.is_zero() {
                    waits.frame = Some(self.timer.sleep(self.frame));
                }
            }
            if let Some(exit) = ready.exit.take() {
                run.exit = Some(exit);
            }
            run.reader_done |= ready.reader_done;
            if run.step(&ready, &mut waits) {
                break;
            }
        }

        // Nothing of the tree outlives the call: whatever is left dies
        // here, and a reader that is still blocked is let go.
        running.stop.stop();
        running.tree.end();
        if !run.reader_done {
            // The reader is on its way out: the tree is dead and its read
            // was stopped. It is told once more, for a read that was just
            // starting when it was first told, and given a moment to hand
            // over what it collected.
            running.stop.stop();
            let mut settle = self.timer.sleep(SETTLE);
            poll_fn(|cx| {
                {
                    let mut state = shared.lock();
                    state.waker = Some(cx.waker().clone());
                    if state.reader_done || state.captured.is_some() {
                        return Poll::Ready(());
                    }
                }
                settle.as_mut().poll(cx)
            })
            .await;
        }
        let (rest, captured) = {
            let mut state = shared.lock();
            (std::mem::take(&mut state.pending), state.captured.take())
        };
        if !rest.is_empty() {
            out.output(&rest);
        }
        Ok(Finished {
            exit: run.exit.and_then(Result::ok),
            ending: run.ending,
            stopped_with: run.stopped_with,
            left_running: run.left_running,
            outlived: run.outlived,
            duration: self.clock.monotonic().saturating_sub(started),
            // A reader that did not finish, or whose store failed under
            // it, has nothing to hand over, and the result says so.
            output_lost: captured.is_none(),
            output: captured.unwrap_or_else(|| Capture::new(&start.label).finish()),
        })
    }

    /// Starts the command and its two threads.
    fn start(
        &self,
        start: &Start,
        store: Arc<dyn ContentStore>,
        shared: &Arc<Shared>,
    ) -> Result<Running, StartError> {
        let launch = &start.launch;
        let mut command = Command::new(&launch.command.program);
        for argument in &launch.command.arguments {
            match argument {
                Argument::Exact(text) => {
                    command.arg(text);
                }
                Argument::Verbatim(text) => {
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        command.raw_arg(text);
                    }
                    #[cfg(not(windows))]
                    command.arg(text);
                }
            }
        }
        command.current_dir(&start.cwd).env_clear().envs(
            start
                .env
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        );
        // No input: the command sees the end of its input at once. Input
        // for a launcher goes through a pipe that is closed behind it.
        let input = match &launch.stdin {
            None => {
                command.stdin(Stdio::null());
                None
            }
            Some(bytes) => {
                let (reader, writer) = std::io::pipe().map_err(StartError::Setup)?;
                command.stdin(reader);
                Some((writer, bytes.clone()))
            }
        };
        let (output, stop) = Output::attach(&mut command).map_err(StartError::Setup)?;
        let tree =
            Tree::spawn(command, self.guard.as_ref()).map_err(|source| StartError::Spawn {
                program: launch.command.program.display().to_string(),
                source,
            })?;
        // From here on, dropping `running` kills the tree.
        let running = Running { tree, stop };

        let reader = {
            let shared = Arc::clone(shared);
            let capture = Capture::new(&start.label);
            std::thread::Builder::new()
                .name("cortex-command-output".to_owned())
                .spawn(move || read_output(output, capture, store.as_ref(), &shared))
        };
        let waiter = {
            let shared = Arc::clone(shared);
            let tree = running.tree.clone();
            std::thread::Builder::new()
                .name("cortex-command-wait".to_owned())
                .spawn(move || {
                    if let Some((mut writer, bytes)) = input {
                        // A launcher that does not read its input is the
                        // launcher's mistake, and shows as a broken pipe.
                        let _ = writer.write_all(&bytes);
                        drop(writer);
                    }
                    let exit = tree.wait().map_err(|error| error.to_string());
                    shared.update(|state| state.exit = Some(exit));
                })
        };
        match (reader, waiter) {
            (Ok(_), Ok(_)) => Ok(running),
            (Err(error), _) | (_, Err(error)) => Err(StartError::Setup(error)),
        }
    }
}

/// A started command. Dropping it kills what is left of the tree and lets
/// the reader go, so that a run that is given up leaves nothing behind.
struct Running {
    tree: Tree,
    stop: OutputStop,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.stop();
        self.tree.end();
    }
}

/// What the two threads tell the future.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Text that has arrived and has not been sent to the front end.
    pending: String,
    /// Whether text was left out of `pending` because it was full.
    pending_cut: bool,
    /// How the command itself ended, once it has.
    exit: Option<Result<Exit, String>>,
    /// Whether the reader thread has ended.
    reader_done: bool,
    /// What the reader collected, once it has ended.
    captured: Option<Captured>,
    waker: Option<Waker>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the state and wakes the future.
    fn update(&self, change: impl FnOnce(&mut State)) {
        let waker = {
            let mut state = self.lock();
            change(&mut state);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Says that the reader has ended, however it ended: a reader that
/// panicked must not leave the future waiting for it.
struct ReaderDone<'a>(&'a Shared);

impl Drop for ReaderDone<'_> {
    fn drop(&mut self) {
        self.0.update(|state| state.reader_done = true);
    }
}

/// The reader thread: the pipe into the buffer, the artifact and the live
/// view, until the pipe closes or the read is stopped.
fn read_output(
    mut output: Output,
    mut capture: Capture,
    store: &dyn ContentStore,
    shared: &Shared,
) {
    let _done = ReaderDone(shared);
    let mut decoder = Utf8Stream::default();
    let mut buffer = vec![0u8; 16 * 1024];
    let mut text = String::new();
    // The store is the session's code, not the runner's. If it panics,
    // the reading goes on with nothing kept: a command must not be left
    // writing into a pipe that nobody reads.
    let mut store_failed = false;
    // A read that fails ends the reading like a closed pipe.
    while let Ok(ReadOutcome::Bytes(count)) = output.read(&mut buffer) {
        let bytes = &buffer[..count];
        if !store_failed {
            let pushed = catch_unwind(AssertUnwindSafe(|| capture.push(bytes, store)));
            store_failed = pushed.is_err();
        }
        text.clear();
        decoder.push(bytes, &mut text);
        if !text.is_empty() {
            shared.update(|state| state.add_pending(&text));
        }
    }
    text.clear();
    decoder.finish(&mut text);
    let captured = if store_failed {
        None
    } else {
        catch_unwind(AssertUnwindSafe(|| capture.finish())).ok()
    };
    shared.update(|state| {
        state.add_pending(&text);
        state.captured = captured;
    });
}

impl State {
    fn add_pending(&mut self, text: &str) {
        if self.pending.len() + text.len() <= PENDING_BYTES {
            self.pending.push_str(text);
        } else if !self.pending_cut {
            // The front end is behind. It is told once, and catches up
            // with the result.
            self.pending_cut = true;
            self.pending
                .push_str("\n[… output is arriving faster than it can be shown …]\n");
        }
    }
}

/// What the future waits for, all at once.
struct Waits<'a> {
    shared: &'a Shared,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    /// The command's time limit. `None` once it has passed.
    timeout: Option<Sleep>,
    /// The end of the current grace. `None` when no grace is running.
    deadline: Option<Sleep>,
    /// The end of the current frame. `None` when output may be sent.
    frame: Option<Sleep>,
}

/// What had happened when a wait ended.
#[derive(Default)]
struct Ready {
    text: Option<String>,
    exit: Option<Result<Exit, String>>,
    reader_done: bool,
    cancelled: bool,
    timed_out: bool,
    deadline: bool,
}

impl Waits<'_> {
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Ready> {
        let mut ready = Ready::default();
        let mut any = false;

        if let Some(frame) = &mut self.frame
            && frame.as_mut().poll(cx).is_ready()
        {
            self.frame = None;
        }
        {
            let mut state = self.shared.lock();
            // The waker is stored before anything is looked at, under the
            // lock the threads change the state under: nothing they do
            // after this is missed.
            state.waker = Some(cx.waker().clone());
            if self.frame.is_none() && !state.pending.is_empty() {
                state.pending_cut = false;
                ready.text = Some(std::mem::take(&mut state.pending));
                any = true;
            }
            if let Some(exit) = state.exit.take() {
                ready.exit = Some(exit);
                any = true;
            }
            if state.reader_done {
                state.reader_done = false;
                ready.reader_done = true;
                any = true;
            }
        }
        if self.cancelled.as_mut().poll(cx).is_ready() {
            // The token stays cancelled: ask once.
            self.cancelled = Box::pin(std::future::pending());
            ready.cancelled = true;
            any = true;
        }
        if let Some(timeout) = &mut self.timeout
            && timeout.as_mut().poll(cx).is_ready()
        {
            self.timeout = None;
            ready.timed_out = true;
            any = true;
        }
        if let Some(deadline) = &mut self.deadline
            && deadline.as_mut().poll(cx).is_ready()
        {
            self.deadline = None;
            ready.deadline = true;
            any = true;
        }
        if any {
            Poll::Ready(ready)
        } else {
            Poll::Pending
        }
    }
}

/// Where a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The command is running, and nothing has asked it to stop.
    Running,
    /// The command has exited by itself, and something it started may
    /// still be running or writing.
    Lingering,
    /// The tree is being stopped, a stage at a time.
    Stopping,
    /// The tree has been killed, or nothing of it was left to kill, and
    /// what is waited for is its exit and the end of its output.
    Killed,
    /// The tree would not be seen to go. The reader has been stopped.
    GivenUp,
}

/// One run's state: what the loop in [`Runner::run`] steps.
struct Run<'a> {
    runner: &'a Runner,
    tree: &'a Tree,
    stop: &'a OutputStop,
    phase: Phase,
    ending: Ending,
    stopped_with: Option<Stop>,
    left_running: bool,
    outlived: bool,
    /// Whether the command has exited and nothing of its tree was found
    /// running since. Nothing can join a tree that is empty.
    nothing_left: bool,
    exit: Option<Result<Exit, String>>,
    reader_done: bool,
}

impl Run<'_> {
    /// Acts on what has happened. `true` when the run is over.
    fn step(&mut self, ready: &Ready, waits: &mut Waits<'_>) -> bool {
        let mut events = (ready.cancelled, ready.timed_out, ready.deadline);
        loop {
            let phase = self.phase;
            if self.act(events, waits) {
                return true;
            }
            if self.phase == phase {
                return false;
            }
            // The run has moved on. Its new phase looks at what is already
            // known: the exit and the end of the output may both have come
            // before it, and then nothing more will arrive to wake it.
            events = (false, false, false);
        }
    }

    /// One phase's part of [`step`](Self::step). The events are whether
    /// the call was cancelled, its time limit passed, or the phase's own
    /// wait ended.
    fn act(
        &mut self,
        (cancelled, timed_out, deadline): (bool, bool, bool),
        waits: &mut Waits<'_>,
    ) -> bool {
        let exited = self.exit.is_some();
        match self.phase {
            Phase::Running => {
                if exited {
                    return self.command_exited(waits);
                }
                if cancelled {
                    self.ending = Ending::Interrupted;
                    self.next_stage(waits);
                } else if timed_out {
                    self.ending = Ending::TimedOut;
                    self.next_stage(waits);
                }
            }
            Phase::Lingering => {
                if self.reader_done && self.nothing_is_left() {
                    return true;
                }
                if deadline || cancelled || timed_out {
                    // Its time is up, or the call's is.
                    if self.nothing_is_left() {
                        // Nothing of the tree is running, and its pipe is
                        // still open: there is nothing to kill, only the
                        // reader to wait a moment more for.
                        self.phase = Phase::Killed;
                        waits.deadline = Some(self.runner.timer.sleep(SETTLE));
                    } else {
                        self.left_running = true;
                        self.kill(waits);
                    }
                }
            }
            Phase::Stopping => {
                if exited && self.reader_done {
                    return true;
                }
                if deadline {
                    self.next_stage(waits);
                }
            }
            Phase::Killed => {
                if exited && self.reader_done {
                    return true;
                }
                if deadline {
                    // The system has not shown the tree gone. With the
                    // command itself gone, what holds the pipe is a
                    // process that left the tree. Stop listening, and give
                    // the reader a moment to say that it has.
                    self.outlived = exited;
                    self.stop.stop();
                    self.phase = Phase::GivenUp;
                    waits.deadline = Some(self.runner.timer.sleep(SETTLE));
                }
            }
            Phase::GivenUp => {
                if self.reader_done || deadline {
                    return true;
                }
            }
        }
        false
    }

    /// The command has exited by itself. Is anything of its tree left?
    fn command_exited(&mut self, waits: &mut Waits<'_>) -> bool {
        // With nothing left, the pipe closes now, unless a process that
        // left the tree holds it.
        if self.reader_done && self.nothing_is_left() {
            return true;
        }
        self.phase = Phase::Lingering;
        waits.deadline = Some(self.runner.timer.sleep(self.runner.linger));
        false
    }

    /// Whether the command that has exited left nothing running. Where the
    /// system will not count, the pipe is the only sign there is, and the
    /// answer is yes.
    fn nothing_is_left(&mut self) -> bool {
        if !self.nothing_left {
            self.nothing_left = self
                .tree
                .others_running()
                .map_or(true, |others| others == 0);
        }
        self.nothing_left
    }

    /// Sends the next stage of stopping. A stage that cannot be sent is
    /// passed over without its grace.
    fn next_stage(&mut self, waits: &mut Waits<'_>) {
        loop {
            let stage = match self.stopped_with {
                None => Stop::Interrupt,
                Some(Stop::Interrupt) => Stop::Terminate,
                Some(Stop::Terminate | Stop::Kill) => Stop::Kill,
            };
            if stage == Stop::Kill {
                self.kill(waits);
                return;
            }
            self.stopped_with = Some(stage);
            let sent = match stage {
                Stop::Interrupt => self.tree.interrupt(),
                Stop::Terminate | Stop::Kill => self.tree.terminate(),
            };
            if sent.is_ok() {
                self.phase = Phase::Stopping;
                waits.deadline = Some(self.runner.timer.sleep(self.runner.grace));
                return;
            }
        }
    }

    fn kill(&mut self, waits: &mut Waits<'_>) {
        if self.ending != Ending::Exited {
            self.stopped_with = Some(Stop::Kill);
        }
        let _ = self.tree.kill();
        self.phase = Phase::Killed;
        // With the command itself gone there is only its pipe to wait for,
        // which closes as the last process of the tree dies.
        let wait = if self.exit.is_some() {
            SETTLE
        } else {
            KILL_WAIT
        };
        waits.deadline = Some(self.runner.timer.sleep(wait));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A wait that ends when the test says so.
    fn manual() -> (Sleep, Arc<AtomicBool>) {
        let over = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&over);
        let sleep = poll_fn(move |_| {
            if flag.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        (Box::pin(sleep), over)
    }

    fn waits(shared: &Shared) -> Waits<'_> {
        Waits {
            shared,
            cancelled: Box::pin(std::future::pending()),
            timeout: None,
            deadline: None,
            frame: None,
        }
    }

    fn poll(waits: &mut Waits<'_>) -> Option<Ready> {
        match waits.poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(ready) => Some(ready),
            Poll::Pending => None,
        }
    }

    #[test]
    fn output_is_sent_at_most_once_a_frame() {
        let shared = Shared::default();
        let mut waits = waits(&shared);
        assert!(poll(&mut waits).is_none(), "nothing has happened");

        // The first piece goes out at once.
        shared.update(|state| state.add_pending("one\n"));
        let ready = poll(&mut waits).expect("there is output");
        assert_eq!(ready.text.as_deref(), Some("one\n"));

        // While a frame is running, what arrives waits and is joined.
        let (frame, over) = manual();
        waits.frame = Some(frame);
        shared.update(|state| state.add_pending("two\n"));
        assert!(poll(&mut waits).is_none(), "the frame is not over");
        shared.update(|state| state.add_pending("three\n"));
        assert!(poll(&mut waits).is_none(), "the frame is not over");

        // The end of the frame sends all of it as one update.
        over.store(true, Ordering::SeqCst);
        let ready = poll(&mut waits).expect("the frame is over");
        assert_eq!(ready.text.as_deref(), Some("two\nthree\n"));
        assert!(waits.frame.is_none());
        assert!(poll(&mut waits).is_none(), "and nothing is sent twice");
    }

    #[test]
    fn the_exit_and_the_end_of_the_output_are_each_told_once() {
        let shared = Shared::default();
        let mut waits = waits(&shared);
        shared.update(|state| {
            state.exit = Some(Ok(Exit::Code(3)));
            state.reader_done = true;
        });
        let ready = poll(&mut waits).expect("two things happened");
        assert_eq!(ready.exit, Some(Ok(Exit::Code(3))));
        assert!(ready.reader_done);
        assert!(!ready.cancelled && !ready.timed_out && !ready.deadline);
        assert!(poll(&mut waits).is_none());

        // The other three, each when its own future completes.
        let (limit, limit_over) = manual();
        let (grace, grace_over) = manual();
        waits.timeout = Some(limit);
        waits.deadline = Some(grace);
        assert!(poll(&mut waits).is_none());
        grace_over.store(true, Ordering::SeqCst);
        let ready = poll(&mut waits).expect("the grace ended");
        assert!(ready.deadline && !ready.timed_out);
        limit_over.store(true, Ordering::SeqCst);
        let ready = poll(&mut waits).expect("the time limit passed");
        assert!(ready.timed_out && !ready.deadline);
        waits.cancelled = Box::pin(std::future::ready(()));
        assert!(poll(&mut waits).expect("cancelled").cancelled);
        assert!(poll(&mut waits).is_none(), "a cancellation is told once");
    }

    #[test]
    fn a_front_end_that_falls_behind_is_told_once_and_holds_a_bounded_amount() {
        let mut state = State::default();
        let piece = "x".repeat(1024);
        for _ in 0..200 {
            state.add_pending(&piece);
        }
        // What fits is kept, then one line says that there is more.
        assert!(
            state.pending.len() <= PENDING_BYTES + 128,
            "{}",
            state.pending.len()
        );
        assert_eq!(
            state.pending.matches("faster than it can be shown").count(),
            1
        );
        assert!(state.pending.starts_with("xxxx"));

        // Once the front end has caught up, output flows again.
        let shared = Shared::default();
        shared.update(|inner| *inner = state);
        let mut waits = waits(&shared);
        assert!(poll(&mut waits).expect("output").text.is_some());
        shared.update(|state| state.add_pending("after\n"));
        assert_eq!(
            poll(&mut waits).expect("output").text.as_deref(),
            Some("after\n")
        );
    }

    #[test]
    fn a_command_line_is_built_a_piece_at_a_time() {
        let line = CommandLine::new("cmd")
            .arg("/c")
            .args(["a", "b"])
            .verbatim("\"x y\"");
        assert_eq!(line.program, PathBuf::from("cmd"));
        assert_eq!(
            line.arguments,
            [
                Argument::Exact("/c".to_owned()),
                Argument::Exact("a".to_owned()),
                Argument::Exact("b".to_owned()),
                Argument::Verbatim("\"x y\"".to_owned()),
            ]
        );
        // The launcher that confines nothing changes nothing.
        let here =
            AbsPath::from_os(&std::env::current_dir().expect("a directory")).expect("a path");
        let launch = Direct
            .launch(line.clone(), &LaunchCx::new(&here, &here))
            .expect("a launch");
        assert_eq!(
            launch,
            Launch {
                command: line,
                stdin: None
            }
        );
        assert_eq!(Direct.explain(Some(Exit::Code(1)), "output"), None);
    }
}
