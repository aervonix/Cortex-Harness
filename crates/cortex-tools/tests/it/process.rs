//! What becomes of a command's processes: on its time limit, on
//! cancellation, when it leaves something running, when the call is given
//! up, and when the harness itself is killed
//! (`docs/architecture/05-tools.md`, "`shell`", "Process tree").
//!
//! The processes are this test binary run again
//! (`cortex_testkit::children`), each seen to start and seen to die
//! through its connection to the test. The time limit and the graces are
//! waits on the test's timer, which end when the test ends them.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use cortex_base::{PathOptions, PathResolver};
use cortex_protocol::item::ToolOutcome;
use cortex_testkit::Gate;
use cortex_testkit::children::{ChildSpec, Roll, until};
use cortex_tools::process::Runner;
use cortex_tools::{
    FileTable, MemoryStore, NoOutput, RunCx, Shell, ShellConfig, ShellKind, ShellTool, Tool,
    execute,
};
use serde_json::json;

use crate::bench::{self, Bench, GRACE, LINGER, default_shell};
use crate::{ENTRY, HARNESS, LIMIT, Workspace, block_on, guard};

/// A command that starts a child, which starts a grandchild that the
/// polite interrupt does not stop.
fn stubborn_family(shell: &Shell, roll: &Roll) -> String {
    bench::child(
        shell,
        &ChildSpec::new(roll, "parent").with_child(ChildSpec::new(roll, "grandchild").stubborn()),
    )
}

#[test]
fn sys_a_time_limit_stops_the_whole_tree() {
    let bench = Bench::new("process_time_limit", default_shell());
    let roll = Roll::new();
    let command = stubborn_family(bench.shell(), &roll);
    let limit = Duration::from_millis(60_000);

    let (ran, ()) = bench.directed(
        json!({ "command": command, "timeout_ms": 60_000 }),
        move |director| {
            let family = roll.all(&["parent", "grandchild"], LIMIT);
            // Nothing stops a command before its time.
            assert!(family[0].is_alive() && family[1].is_alive());
            assert!(director.timer.running().contains(&limit));
            assert_eq!(director.timer.end(limit), 1);
            // The grandchild sits out the polite stages, and each grace is
            // ended as it starts, until the kill has taken everything.
            director.ending_each(GRACE, "the tree to be gone", || {
                !family[0].is_alive() && !family[1].is_alive()
            });
        },
    );
    assert_eq!(ran.result.outcome(), &ToolOutcome::Error);
    let text = ran.text();
    assert!(text.contains("[timed out after "), "{text}");
    assert!(text.contains(" and was stopped: "), "{text}");
    assert_eq!(ran.payload("timed_out"), json!(true));
    assert_eq!(ran.payload("interrupted"), json!(false));
    assert_eq!(ran.payload("stopped_with"), json!("kill"));
    assert_eq!(ran.payload("background_stopped"), json!(false));
}

#[test]
fn sys_cancelling_a_call_stops_the_whole_tree() {
    let bench = Bench::new("process_cancel", default_shell());
    let roll = Roll::new();
    let command = stubborn_family(bench.shell(), &roll);

    let (ran, ()) = bench.directed(json!({ "command": command }), move |director| {
        let family = roll.all(&["parent", "grandchild"], LIMIT);
        director.cancel.cancel();
        director.ending_each(GRACE, "the tree to be gone", || {
            !family[0].is_alive() && !family[1].is_alive()
        });
    });
    // A cancelled call still has a result, with what there was to show.
    assert_eq!(ran.result.outcome(), &ToolOutcome::Interrupted);
    assert!(ran.text().contains("[interrupted after "), "{}", ran.text());
    assert_eq!(ran.payload("interrupted"), json!(true));
    assert_eq!(ran.payload("timed_out"), json!(false));
    assert_eq!(ran.payload("stopped_with"), json!("kill"));
}

#[test]
fn sys_the_polite_interrupt_is_enough_for_a_command_that_listens() {
    let bench = Bench::new("process_polite", default_shell());
    let roll = Roll::new();
    let command = bench::child(
        bench.shell(),
        &ChildSpec::new(&roll, "only").saying("started"),
    );

    let live = bench::Live::default();
    let watching = live.clone();
    let (ran, graces_ended) =
        bench.directed_into(json!({ "command": command }), &live, move |director| {
            let only = roll.next(LIMIT);
            // It says its word after it has reported, so the word is waited
            // for: an interrupt a moment earlier would stop it first.
            until(LIMIT, "the command's word", || {
                watching.text().contains("started")
            });
            director.cancel.cancel();
            // No grace is ended: the interrupt alone must do it.
            assert!(only.gone_within(LIMIT), "the command ignored the interrupt");
            until(LIMIT, "the call to end", || director.call_has_ended());
            director.timer.end(GRACE)
        });
    assert_eq!(ran.result.outcome(), &ToolOutcome::Interrupted);
    assert_eq!(ran.payload("stopped_with"), json!("interrupt"));
    assert_eq!(graces_ended, 0, "the run was over before its grace");
    // What it had printed before it was stopped is in the result.
    assert!(ran.text().contains("started"), "{}", ran.text());
}

/// A call that was cancelled before its turn came starts nothing, whether
/// or not it would have looked for changed files first.
#[test]
fn sys_a_call_cancelled_before_it_starts_runs_no_command() {
    for looks in [false, true] {
        let bench = Bench::with(
            &format!("process_cancelled_first_{looks}"),
            default_shell(),
            |config| config.detect_changes = looks,
        );
        let roll = Roll::new();
        let command = bench::child(bench.shell(), &ChildSpec::new(&roll, "never").exiting(0));
        let cancel = cortex_base::CancelToken::new();
        cancel.cancel();
        let cx = bench.ws.cx().with_cancel(cancel);
        let plan = bench
            .tool
            .plan(&json!({ "command": command }), &cx.plan_cx())
            .expect("a plan");
        let result = block_on(execute(&bench.tool, plan, &cx, &mut NoOutput));
        assert_eq!(result.outcome(), &ToolOutcome::Interrupted);
        assert_eq!(
            result.text(),
            "`shell` was interrupted before the command started."
        );
        assert!(roll.try_next().is_none(), "the command was started");
    }
}

/// Output reaches the front end while the command runs, not when it has
/// ended.
#[test]
fn sys_output_is_streamed_while_the_command_runs() {
    let bench = Bench::new("process_streaming", default_shell());
    let roll = Roll::new();
    // It says a word and then stays until it is stopped.
    let command = bench::child(
        bench.shell(),
        &ChildSpec::new(&roll, "talker").saying("said-while-running"),
    );
    let live = bench::Live::default();
    let watching = live.clone();
    let (ran, updates_before_the_end) =
        bench.directed_into(json!({ "command": command }), &live, move |director| {
            let talker = roll.next(LIMIT);
            until(LIMIT, "the word to be streamed", || {
                watching.text().contains("said-while-running")
            });
            // It arrived, and the command is still running.
            assert!(talker.is_alive());
            assert!(!director.call_has_ended());
            let updates = watching.updates();
            director.cancel.cancel();
            assert!(talker.gone_within(LIMIT));
            updates
        });
    assert!(updates_before_the_end >= 1);
    assert_eq!(ran.result.outcome(), &ToolOutcome::Interrupted);
    // The result holds it too.
    assert!(ran.text().contains("said-while-running"), "{}", ran.text());
}

/// A content store that panics when an artifact is begun.
struct BrokenStore;

impl cortex_tools::ContentStore for BrokenStore {
    fn begin_artifact(
        &self,
        _label: &str,
        _media_type: &str,
    ) -> Result<Box<dyn cortex_tools::ArtifactWriter>, cortex_tools::StoreError> {
        panic!("the content store is broken, on purpose");
    }

    fn open_artifact(
        &self,
        id: &cortex_protocol::ids::ArtifactId,
    ) -> Result<Box<dyn std::io::Read + Send>, cortex_tools::StoreError> {
        Err(cortex_tools::StoreError::NoSuchArtifact(id.to_string()))
    }

    fn put_version(
        &self,
        content: &[u8],
    ) -> Result<cortex_base::ContentHash, cortex_tools::StoreError> {
        Ok(cortex_base::ContentHash::of(content))
    }

    fn get_version(
        &self,
        _hash: &cortex_base::ContentHash,
    ) -> Result<Option<Vec<u8>>, cortex_tools::StoreError> {
        Ok(None)
    }
}

/// A store that panics under the reader must not leave the command
/// writing into a pipe nobody reads, and must not be reported as a
/// command that printed nothing.
#[test]
fn sys_a_store_that_panics_loses_the_output_and_not_the_command() {
    let bench = Bench::new("process_broken_store", default_shell());
    let cx = RunCx::new(
        Arc::clone(&bench.ws.paths),
        Arc::new(BrokenStore),
        Arc::clone(&bench.ws.files),
    );
    // More than the buffer holds, so that an artifact is begun, and more
    // than a pipe holds, so that a reader that stopped would block it.
    let command = bench::count_lines(bench.shell(), 20_000);
    let plan = bench
        .tool
        .plan(&json!({ "command": command }), &cx.plan_cx())
        .expect("a plan");
    let result = block_on(execute(&bench.tool, plan, &cx, &mut NoOutput));
    let text = result.text();
    assert_eq!(result.outcome(), &ToolOutcome::Success, "{text}");
    assert!(text.contains("could not be collected"), "{text}");
    assert!(!text.contains("(no output)"), "{text}");
    assert!(text.contains("[exit code 0, after "), "{text}");
    let payload = result.payload().expect("a payload");
    assert_eq!(payload["output_lost"], json!(true));
    assert_eq!(payload["exit_code"], json!(0));
}

/// The three stages, one at a time, on a command that ignores the first
/// two. Windows has two stages, and which of them ends a shell depends on
/// the shell, so this is Linux's test.
#[cfg(unix)]
#[test]
fn sys_stopping_goes_from_interrupt_to_terminate_to_kill() {
    let Some(shell) = bench::shell("bash") else {
        return;
    };
    let bench = Bench::new("process_stages", shell);
    let roll = Roll::new();
    let command = format!(
        "trap '' INT TERM; {}",
        bench::child(bench.shell(), &ChildSpec::new(&roll, "deaf"))
    );
    let (ran, ()) = bench.directed(json!({ "command": command }), move |director| {
        let deaf = roll.next(LIMIT);
        director.cancel.cancel();
        // The grace starts once the interrupt has been sent.
        director.timer.end_when_running(GRACE);
        assert!(deaf.is_alive(), "it ignores the interrupt");
        director.timer.end_when_running(GRACE);
        assert!(deaf.gone_within(LIMIT), "nothing ignores the kill");
    });
    assert_eq!(ran.payload("stopped_with"), json!("kill"));
    assert_eq!(ran.payload("signal"), json!(9));
    assert!(
        ran.text()
            .contains("and was stopped: killed by signal 9 (SIGKILL)]"),
        "{}",
        ran.text()
    );

    // One that ignores the interrupt alone is ended by the second stage.
    let roll = Roll::new();
    let command = format!(
        "trap '' INT; {}",
        bench::child(bench.shell(), &ChildSpec::new(&roll, "hard-of-hearing"))
    );
    let (ran, ()) = bench.directed(json!({ "command": command }), move |director| {
        let child = roll.next(LIMIT);
        director.cancel.cancel();
        director.timer.end_when_running(GRACE);
        assert!(
            child.gone_within(LIMIT),
            "it does not ignore the second stage"
        );
    });
    assert_eq!(ran.payload("stopped_with"), json!("terminate"));
    assert_eq!(ran.payload("signal"), json!(15));
}

/// A command that exits and leaves a process holding its output: the tool
/// stops waiting after a short grace, stops the process, and says so. Run
/// for every shell, from `shell.rs`.
pub(crate) fn a_background_process_is_stopped(test: &str, shell: Shell) {
    let bench = Bench::new(test, shell);
    let roll = Roll::new();
    let command = bench::in_background(bench.shell(), &ChildSpec::new(&roll, "left-behind"));

    let (ran, ()) = bench.directed(json!({ "command": command }), move |director| {
        let left = roll.next(LIMIT);
        // The grace starts when the command itself has exited.
        director.timer.end_when_running(LINGER);
        assert!(left.gone_within(LIMIT), "it was left running");
    });
    // The command itself succeeded, and the result says what was done.
    assert_eq!(
        ran.result.outcome(),
        &ToolOutcome::Success,
        "{}",
        ran.text()
    );
    assert_eq!(ran.payload("exit_code"), json!(0));
    assert_eq!(ran.payload("background_stopped"), json!(true));
    let text = ran.text();
    assert!(
        text.contains("[The command left processes running, and they were stopped."),
        "{text}"
    );
    assert!(
        text.contains("Background processes are not supported yet"),
        "{text}"
    );
}

/// The same when the process does not hold the output: it redirected its
/// own, as a server started "properly" does. The pipe closes at once, and
/// the process is still found, stopped and reported.
#[test]
fn sys_a_background_process_with_its_own_output_is_stopped_too() {
    let shell = if cfg!(windows) {
        match bench::shell("cmd") {
            Some(shell) => shell,
            None => return,
        }
    } else {
        default_shell()
    };
    let bench = Bench::new("process_background_quiet", shell);
    let roll = Roll::new();
    let child = bench::child(bench.shell(), &ChildSpec::new(&roll, "quiet"));
    let command = match bench.shell().kind() {
        ShellKind::Cmd => format!("start /b \"\" {child} >NUL 2>&1"),
        _ => format!("{child} >/dev/null 2>&1 &"),
    };
    let (ran, ()) = bench.directed(json!({ "command": command }), move |director| {
        let quiet = roll.next(LIMIT);
        director.timer.end_when_running(LINGER);
        assert!(quiet.gone_within(LIMIT), "it was left running");
    });
    assert_eq!(
        ran.payload("background_stopped"),
        json!(true),
        "{}",
        ran.text()
    );
    assert!(ran.text().contains("they were stopped"), "{}", ran.text());
}

/// A command whose last process is a moment behind it is not a command
/// that left something running.
#[cfg(target_os = "linux")]
#[test]
fn sys_a_process_that_finishes_by_itself_is_not_reported() {
    let bench = Bench::new("process_straggler", default_shell());
    let roll = Roll::new();
    let straggler = bench::child(
        bench.shell(),
        &ChildSpec::new(&roll, "straggler").saying("late").exiting(0),
    );
    let (ran, ()) = bench.directed(
        json!({ "command": format!("{straggler} & echo now") }),
        move |director| {
            let late = roll.next(LIMIT);
            // Gone for good, and not just on its way out: no longer in the
            // process table, or in it only as an exit waiting to be read.
            let entry = format!("/proc/{}/stat", late.pid());
            until(LIMIT, "the straggler to be gone", || {
                std::fs::read_to_string(&entry).map_or(true, |stat| {
                    stat.rsplit_once(") ")
                        .is_some_and(|(_, rest)| rest.starts_with('Z'))
                })
            });
            // If the command's exit caught it on its way out, the grace
            // is what settles it, and settles it as gone.
            director.ending_each(LINGER, "the call to end", || director.call_has_ended());
        },
    );
    assert_eq!(
        ran.result.outcome(),
        &ToolOutcome::Success,
        "{}",
        ran.text()
    );
    assert_eq!(
        ran.payload("background_stopped"),
        json!(false),
        "{}",
        ran.text()
    );
    assert_eq!(ran.payload("outlived"), json!(false), "{}", ran.text());
    let text = ran.text();
    assert!(text.contains("late") && text.contains("now"), "{text}");
    assert!(!text.contains("left processes running"), "{text}");
}

/// A process that leaves the command's process group cannot be stopped,
/// and the result says that, and not that it was.
#[cfg(target_os = "linux")]
#[test]
fn sys_a_process_that_left_the_group_is_reported_as_still_running() {
    if !std::path::Path::new("/usr/bin/setsid").exists() {
        crate::skipped("there is no setsid program here");
        return;
    }
    let bench = Bench::new("process_escaped", default_shell());
    let roll = Roll::new();
    let escaped = bench::child(bench.shell(), &ChildSpec::new(&roll, "escaped"));
    let (ran, escaped) = bench.directed(
        json!({ "command": format!("/usr/bin/setsid {escaped} & echo started") }),
        move |director| {
            let escaped = roll.next(LIMIT);
            // The command exits, nothing of its group is left, and its
            // output is still held: first the grace, then the moment the
            // runner gives the output to end.
            director.timer.end_when_running(LINGER);
            director
                .timer
                .end_when_running(cortex_tools::process::SETTLE);
            escaped
        },
    );
    assert_eq!(
        ran.result.outcome(),
        &ToolOutcome::Success,
        "{}",
        ran.text()
    );
    assert_eq!(ran.payload("outlived"), json!(true), "{}", ran.text());
    assert_eq!(ran.payload("background_stopped"), json!(false));
    let text = ran.text();
    assert!(text.contains("started"), "{text}");
    assert!(
        text.contains("is still running and still holds the command's output"),
        "{text}"
    );
    assert!(text.contains("could not be stopped"), "{text}");
    assert!(!text.contains("they were stopped"), "{text}");

    // It is, and the test ends it: this is the limit that the sandbox is
    // there for.
    assert!(escaped.is_alive());
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &escaped.pid().to_string()])
        .status()
        .expect("run kill");
    assert!(killed.success());
    assert!(escaped.gone_within(LIMIT));
}

/// Completes with the first of two futures, and drops the other.
async fn first<A: Future, B: Future>(one: A, other: B) -> Option<A::Output> {
    let mut one = pin!(one);
    let mut other = pin!(other);
    poll_fn(|cx| {
        if let Poll::Ready(output) = one.as_mut().poll(cx) {
            return Poll::Ready(Some(output));
        }
        if other.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

#[test]
fn sys_a_call_that_is_given_up_kills_its_command() {
    let bench = Bench::new("process_dropped", default_shell());
    let roll = Roll::new();
    let command = stubborn_family(bench.shell(), &roll);
    let cx = bench.ws.cx();
    let plan = bench
        .tool
        .plan(&json!({ "command": command }), &cx.plan_cx())
        .expect("a plan");

    // The call's future is dropped as soon as both processes are up: no
    // cancellation, no time limit, nobody waiting for it.
    let up = Gate::new();
    let opener = up.clone();
    let watcher = std::thread::spawn(move || {
        let family = roll.all(&["parent", "grandchild"], LIMIT);
        opener.open();
        family
    });
    let ended = block_on(first(
        execute(&bench.tool, plan, &cx, &mut NoOutput),
        up.opened(),
    ));
    assert!(ended.is_none(), "the command does not end by itself");
    let family = watcher.join().expect("the watcher");
    assert!(
        family[0].gone_within(LIMIT),
        "the command outlived its call"
    );
    assert!(
        family[1].gone_within(LIMIT),
        "the grandchild outlived its call"
    );
}

/// The harness of [`sys_a_killed_harness_leaves_nothing_of_its_command`]:
/// this binary, run again to run one command through the tool for as long
/// as it lives. `arguments` are `--`, a workspace and a command.
pub(crate) fn be_a_harness(arguments: &[String]) -> ! {
    let [_, workspace, command] = arguments else {
        panic!("a harness is given a workspace and a command: {arguments:?}");
    };
    let paths = Arc::new(
        PathResolver::new(std::path::Path::new(workspace), PathOptions::new())
            .expect("open the workspace"),
    );
    let cx = RunCx::new(
        paths,
        Arc::new(MemoryStore::new()),
        Arc::new(FileTable::new()),
    );
    let config = ShellConfig {
        detect_changes: false,
        ..ShellConfig::default()
    };
    // As a real harness runs it: the runtime's own timer, and the guard.
    let tool =
        ShellTool::new(default_shell(), config).with_runner(Runner::new().with_guard(guard()));
    let plan = tool
        .plan(&json!({ "command": command }), &cx.plan_cx())
        .expect("a plan");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime");
    let result = runtime.block_on(execute(&tool, plan, &cx, &mut NoOutput));
    panic!("the command ended, and it never should: {}", result.text());
}

#[test]
fn sys_a_killed_harness_leaves_nothing_of_its_command() {
    let ws = Workspace::new("process_killed_harness");
    let roll = Roll::new();
    let command = stubborn_family(&default_shell(), &roll);
    let mut harness = std::process::Command::new(std::env::current_exe().expect("a path"))
        .args(["--exact", ENTRY, "--nocapture", HARNESS, "--"])
        .arg(ws.root())
        .arg(&command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the harness");
    let family = roll.all(&["parent", "grandchild"], LIMIT);

    // No warning and no clean-up: the harness is simply gone.
    harness.kill().expect("kill the harness");
    harness.wait().expect("reap the harness");

    assert!(
        family[0].gone_within(LIMIT),
        "the command outlived the harness"
    );
    assert!(
        family[1].gone_within(LIMIT),
        "the grandchild outlived the harness"
    );
}
