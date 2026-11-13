//! What the tests of the `shell` tool share: a timer that the test moves, a
//! bench that holds a workspace with the tool, and the few commands that
//! are written differently for each shell.
//!
//! # Time
//!
//! The runner waits on [`TestTimer`], whose waits end only when the test
//! ends them, by their length: the time limit of a call, the grace between
//! two stages of stopping, the grace for what a command left running. So a
//! test of a two-minute time limit takes as long as the processes take to
//! die, and nothing in a test sleeps.
//!
//! # Two threads
//!
//! A call that does not end by itself needs something done while it runs:
//! wait for a child to report, end a wait, cancel. [`Bench::directed`] runs
//! the call on the test's thread and a director on another. A director
//! that panics lets every wait go and cancels the call, so that a failed
//! assertion is a failed test and not a hung one.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use cortex_base::CancelToken;
use cortex_testkit::Gate;
use cortex_testkit::children::{ChildSpec, until};
use cortex_tools::process::{Runner, Sleep, Timer};
use cortex_tools::{
    Json, Shell, ShellConfig, ShellKind, ShellSearch, ShellTool, Tool, ToolResult, execute,
};

use crate::{ENTRY, LIMIT, Workspace, block_on, guard, skipped};

/// The grace between two stages of stopping, in these tests.
pub(crate) const GRACE: Duration = Duration::from_secs(2);
/// The grace for what a command left running, in these tests.
pub(crate) const LINGER: Duration = Duration::from_millis(500);

/// A [`Timer`] whose waits end when the test says so.
#[derive(Debug, Clone, Default)]
pub(crate) struct TestTimer {
    state: Arc<Mutex<TimerState>>,
}

#[derive(Debug, Default)]
struct TimerState {
    next: u64,
    waits: Vec<Wait>,
    /// The length of every wait that was ever started, in order.
    started: Vec<Duration>,
    /// Every wait ends at once: the test is over, or has failed.
    released: bool,
}

#[derive(Debug)]
struct Wait {
    id: u64,
    length: Duration,
    over: bool,
    waker: Option<Waker>,
}

struct TestSleep {
    timer: TestTimer,
    id: u64,
}

impl Timer for TestTimer {
    fn sleep(&self, duration: Duration) -> Sleep {
        let mut state = self.state.lock().unwrap();
        let id = state.next;
        state.next += 1;
        let over = state.released;
        state.started.push(duration);
        state.waits.push(Wait {
            id,
            length: duration,
            over,
            waker: None,
        });
        Box::pin(TestSleep {
            timer: self.clone(),
            id,
        })
    }
}

impl Future for TestSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.timer.state.lock().unwrap();
        let wait = state
            .waits
            .iter_mut()
            .find(|wait| wait.id == self.id)
            .expect("a wait is there until it is dropped");
        if wait.over {
            Poll::Ready(())
        } else {
            wait.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Drop for TestSleep {
    fn drop(&mut self) {
        let mut state = self.timer.state.lock().unwrap();
        state.waits.retain(|wait| wait.id != self.id);
    }
}

impl TestTimer {
    /// The lengths of every wait that was ever started, in order.
    pub(crate) fn started(&self) -> Vec<Duration> {
        self.state.lock().unwrap().started.clone()
    }

    /// The lengths of the waits that are running.
    pub(crate) fn running(&self) -> Vec<Duration> {
        let state = self.state.lock().unwrap();
        state
            .waits
            .iter()
            .filter(|wait| !wait.over)
            .map(|wait| wait.length)
            .collect()
    }

    /// Ends every running wait of this length, and says how many it ended.
    pub(crate) fn end(&self, length: Duration) -> usize {
        let wakers: Vec<Option<Waker>> = {
            let mut state = self.state.lock().unwrap();
            state
                .waits
                .iter_mut()
                .filter(|wait| !wait.over && wait.length == length)
                .map(|wait| {
                    wait.over = true;
                    wait.waker.take()
                })
                .collect()
        };
        let ended = wakers.len();
        for waker in wakers.into_iter().flatten() {
            waker.wake();
        }
        ended
    }

    /// Waits until a wait of this length is running, and ends it. The
    /// runner starts a grace right after it has sent a stage, so this
    /// returns when the stage has been sent.
    pub(crate) fn end_when_running(&self, length: Duration) {
        until(LIMIT, &format!("a wait of {length:?} to start"), || {
            self.running().contains(&length)
        });
        assert!(self.end(length) >= 1);
    }

    /// Ends every wait there is and every wait there will be.
    pub(crate) fn release(&self) {
        let wakers: Vec<Option<Waker>> = {
            let mut state = self.state.lock().unwrap();
            state.released = true;
            state
                .waits
                .iter_mut()
                .map(|wait| {
                    wait.over = true;
                    wait.waker.take()
                })
                .collect()
        };
        for waker in wakers.into_iter().flatten() {
            waker.wake();
        }
    }
}

/// What a director works with: the timer and the call's token. If the
/// director panics, the call is let go.
pub(crate) struct Director {
    pub(crate) timer: TestTimer,
    pub(crate) cancel: CancelToken,
    /// The call's second token: "kill now".
    pub(crate) kill: CancelToken,
    finished: Arc<AtomicBool>,
}

impl Director {
    /// Whether the call has returned.
    pub(crate) fn call_has_ended(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// Ends every wait of `length` as it starts, until `done` holds. For a
    /// run whose number of stages differs between systems.
    pub(crate) fn ending_each(&self, length: Duration, what: &str, mut done: impl FnMut() -> bool) {
        until(LIMIT, what, || {
            self.timer.end(length);
            done()
        });
    }
}

impl Drop for Director {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.cancel.cancel();
            self.timer.release();
        }
    }
}

/// Where a call's streamed output goes: text that another thread can read
/// while the call runs, with how many pieces it arrived in.
#[derive(Debug, Clone, Default)]
pub(crate) struct Live {
    seen: Arc<Mutex<(String, usize)>>,
}

impl Live {
    /// What has been streamed so far.
    pub(crate) fn text(&self) -> String {
        self.seen.lock().unwrap().0.clone()
    }

    /// In how many updates.
    pub(crate) fn updates(&self) -> usize {
        self.seen.lock().unwrap().1
    }
}

impl cortex_tools::ToolOutput for Live {
    fn output(&mut self, text: &str) {
        let mut seen = self.seen.lock().unwrap();
        seen.0.push_str(text);
        seen.1 += 1;
    }
}

/// A call's result with what it streamed while it ran.
pub(crate) struct Ran {
    pub(crate) result: ToolResult,
    pub(crate) streamed: String,
}

impl Ran {
    pub(crate) fn text(&self) -> String {
        // The shells of Windows end a line with a carriage return too.
        self.result.text().replace("\r\n", "\n")
    }

    pub(crate) fn payload(&self, field: &str) -> Json {
        self.result
            .payload()
            .expect("a shell result has a payload")
            .get(field)
            .unwrap_or_else(|| panic!("the payload has no `{field}`"))
            .clone()
    }
}

/// A workspace with the `shell` tool for one shell, on a test timer.
pub(crate) struct Bench {
    pub(crate) ws: Workspace,
    pub(crate) tool: ShellTool,
    pub(crate) timer: TestTimer,
}

impl Bench {
    /// A bench that does not look for changed files: most tests are about
    /// the command, and the workspace of a test is inside this repository.
    pub(crate) fn new(test: &str, shell: Shell) -> Self {
        Self::with(test, shell, |config| config.detect_changes = false)
    }

    pub(crate) fn with(test: &str, shell: Shell, configure: impl FnOnce(&mut ShellConfig)) -> Self {
        Self::in_workspace(Workspace::new(test), shell, configure)
    }

    pub(crate) fn in_workspace(
        ws: Workspace,
        shell: Shell,
        configure: impl FnOnce(&mut ShellConfig),
    ) -> Self {
        let timer = TestTimer::default();
        let mut config = ShellConfig::default();
        configure(&mut config);
        let tool = ShellTool::new(shell, config).with_runner(runner(&timer));
        Self { ws, tool, timer }
    }

    pub(crate) fn shell(&self) -> &Shell {
        self.tool.shell()
    }

    /// Runs a call that ends by itself.
    pub(crate) fn run(&self, arguments: Json) -> Ran {
        self.run_with(arguments, &CancelToken::new())
    }

    /// Runs a command, with nothing else in the call.
    pub(crate) fn command(&self, command: &str) -> Ran {
        self.run(serde_json::json!({ "command": command }))
    }

    /// Runs a call that must end by itself, with nobody to end a wait for
    /// it. If it has not ended within [`LIMIT`], every wait is let go and
    /// the test fails saying which waits the runner was in: a hand-driven
    /// timer otherwise turns a runner that waits for something that is
    /// not there into a test that is stopped a minute later with nothing
    /// to read. (That is how a Job Object that still counted a command
    /// after its exit showed, one run in seven.)
    fn run_with(&self, arguments: Json, cancel: &CancelToken) -> Ran {
        let ended = Gate::new();
        let watcher = {
            let (ended, timer) = (ended.clone(), self.timer.clone());
            std::thread::spawn(move || {
                if ended.wait(LIMIT) {
                    return None;
                }
                let waits = timer.running();
                timer.release();
                Some(waits)
            })
        };
        let ran = self.run_into(arguments, cancel, &CancelToken::new(), &Live::default());
        ended.open();
        let stuck = match watcher.join() {
            Ok(stuck) => stuck,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        if let Some(waits) = stuck {
            panic!(
                "the call did not end by itself within {LIMIT:?}. The runner was waiting \
                 {waits:?} long, on a timer that only a test's director ends: beside the \
                 time limit, a wait of {LINGER:?} is the grace for what a command left \
                 running, and one of {:?} is for output that did not end. The result, \
                 once the waits were let go:\n{}",
                cortex_tools::process::SETTLE,
                ran.text()
            );
        }
        ran
    }

    fn run_into(
        &self,
        arguments: Json,
        cancel: &CancelToken,
        kill: &CancelToken,
        live: &Live,
    ) -> Ran {
        let cx = self
            .ws
            .cx()
            .with_cancel(cancel.clone())
            .with_kill(kill.clone());
        let plan = self
            .tool
            .plan(&arguments, &cx.plan_cx())
            .unwrap_or_else(|error| panic!("the call could not be planned: {error}"));
        let mut out = live.clone();
        let result = block_on(execute(&self.tool, plan, &cx, &mut out));
        let streamed = live.text();
        assert!(
            result.is_bounded(),
            "a result that skipped the output policy"
        );
        assert!(
            cx.limits.fits(&result.text()),
            "a result over the limits:\n{}",
            result.text()
        );
        Ran { result, streamed }
    }

    /// Runs a call on this thread while `direct` runs on another, and
    /// returns both results. The director is given the timer and the
    /// call's token.
    pub(crate) fn directed<T: Send + 'static>(
        &self,
        arguments: Json,
        direct: impl FnOnce(&Director) -> T + Send + 'static,
    ) -> (Ran, T) {
        self.directed_into(arguments, &Live::default(), direct)
    }

    /// [`directed`](Self::directed), with what the call streams going to
    /// `live`, which the director may hold a clone of and read while the
    /// call runs.
    pub(crate) fn directed_into<T: Send + 'static>(
        &self,
        arguments: Json,
        live: &Live,
        direct: impl FnOnce(&Director) -> T + Send + 'static,
    ) -> (Ran, T) {
        let cancel = CancelToken::new();
        let kill = CancelToken::new();
        let finished = Arc::new(AtomicBool::new(false));
        let director = Director {
            timer: self.timer.clone(),
            cancel: cancel.clone(),
            kill: kill.clone(),
            finished: Arc::clone(&finished),
        };
        let thread = std::thread::spawn(move || direct(&director));
        let ran = self.run_into(arguments, &cancel, &kill, live);
        finished.store(true, Ordering::SeqCst);
        let directed = match thread.join() {
            Ok(directed) => directed,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        (ran, directed)
    }
}

/// A runner on a test timer, with the guard and one update for each piece
/// of output.
pub(crate) fn runner(timer: &TestTimer) -> Runner {
    Runner::new()
        .with_timer(Arc::new(timer.clone()))
        .with_guard(guard())
        .with_grace(GRACE)
        .with_linger(LINGER)
        .with_frame(Duration::ZERO)
}

/// The shell called `name` on this machine, or `None` after saying that
/// the test has nothing to run with, which fails in CI.
pub(crate) fn shell(name: &str) -> Option<Shell> {
    match ShellSearch::from_env().find(Some(name), None) {
        Ok(shell) => Some(shell),
        Err(error) => {
            skipped(&error.to_string());
            None
        }
    }
}

/// The shell this system gives when nothing is configured.
pub(crate) fn default_shell() -> Shell {
    ShellSearch::from_env()
        .find(None, None)
        .expect("every system these tests run on has a shell")
}

/// The command that runs a program with arguments, in a shell's syntax.
/// The program's path is quoted by the shell's own rules; the arguments
/// are words that need none.
pub(crate) fn invoke(shell: &Shell, program: &str, arguments: &[String]) -> String {
    let quoted = shell
        .quote(program)
        .expect("the test binary's path can be quoted");
    let call = match shell.kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => format!("& {quoted}"),
        _ => quoted,
    };
    format!("{call} {}", arguments.join(" "))
}

/// The command that runs a child, in a shell's syntax.
pub(crate) fn child(shell: &Shell, spec: &ChildSpec) -> String {
    let (program, arguments) = spec.arguments(ENTRY);
    invoke(shell, &program, &arguments)
}

/// The command that starts a child and does not wait for it, leaving the
/// child the command's output.
pub(crate) fn in_background(shell: &Shell, spec: &ChildSpec) -> String {
    let (program, arguments) = spec.arguments(ENTRY);
    match shell.kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => format!(
            "Start-Process -NoNewWindow -FilePath {} -ArgumentList {}",
            shell.quote(&program).expect("quotable"),
            arguments
                .iter()
                .map(|argument| shell.quote(argument).expect("quotable"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        ShellKind::Cmd => format!("start /b \"\" {}", invoke(shell, &program, &arguments)),
        _ => format!("{} &", invoke(shell, &program, &arguments)),
    }
}

/// The command that exits with a code.
pub(crate) fn exit_with(shell: &Shell, code: i32) -> String {
    match shell.kind() {
        ShellKind::Cmd => format!("exit /b {code}"),
        _ => format!("exit {code}"),
    }
}

/// The command that prints the value of an environment variable on a line.
pub(crate) fn print_variable(shell: &Shell, name: &str) -> String {
    match shell.kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            format!("[Console]::Out.WriteLine($env:{name})")
        }
        ShellKind::Cmd => format!("echo %{name}%"),
        _ => format!("printf '%s\\n' \"${name}\""),
    }
}

/// The command that prints `line 1` to `line <count>`, one to a line.
pub(crate) fn count_lines(shell: &Shell, count: usize) -> String {
    match shell.kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            format!("1..{count} | ForEach-Object {{ \"line $_\" }}")
        }
        ShellKind::Cmd => format!("for /L %i in (1,1,{count}) do @echo line %i"),
        _ => format!("i=1; while [ $i -le {count} ]; do echo \"line $i\"; i=$((i+1)); done"),
    }
}

/// The names in the working directory, one to a line.
pub(crate) fn list_names(shell: &Shell) -> &'static str {
    match shell.kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => "Get-ChildItem -Name",
        ShellKind::Cmd => "dir /b",
        _ => "ls",
    }
}
