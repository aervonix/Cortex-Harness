//! System tests (tier 3) of process trees: real processes, started, stopped
//! and killed.
//!
//! The children are this test binary run again
//! (`cortex_testkit::children`): each reports to the test over a loopback
//! socket and keeps the connection until it dies, so a test sees a process
//! start and sees it go, and never waits a fixed time. Every limit below is
//! a bound on a failure.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a process test starts processes, fails by panicking, and says what it measured"
)]

mod tree;

use std::process::Command;
use std::time::Duration;

use cortex_sandbox::tree::{Guard, Tree, run_guard};
use cortex_testkit::children::{ChildSpec, run_if_child};

/// The test that a child process enters by.
const ENTRY: &str = "child_entry";

/// The argument that makes this binary the guard.
const GUARD: &str = "cx-guard";

/// The arguments that make this binary a harness that starts one tree and
/// then stays until it is killed: with the guard, and without it.
const HARNESS: &str = "cx-harness";
const BARE_HARNESS: &str = "cx-harness-bare";

/// The argument that makes this binary start one tree, wait for its
/// command, and exit with [`WAITED`] plus the command's exit code.
const WAITER: &str = "cx-waiter";

/// What a waiter adds to the exit code it reports, so that the code is
/// plainly a waiter's answer.
const WAITED: i32 = 40;

/// How long a test lets something take before it calls it a failure.
const LIMIT: Duration = Duration::from_secs(30);

/// What the test runner runs as an ordinary test, which passes at once, and
/// what a test runs again as a child, as the guard, or as a harness.
#[test]
fn child_entry() {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.iter().any(|argument| argument == GUARD) {
        run_guard();
        return;
    }
    let harness = arguments
        .iter()
        .position(|argument| argument == HARNESS || argument == BARE_HARNESS || argument == WAITER);
    if let Some(at) = harness {
        // Everything after `--` is the command of the one tree.
        let rest = arguments
            .iter()
            .position(|argument| argument == "--")
            .expect("a harness is given a command");
        let mut command = Command::new(&arguments[rest + 1]);
        command.args(&arguments[rest + 2..]);
        if arguments[at] == WAITER {
            let tree = Tree::spawn(command, None).expect("start the tree");
            let code = match tree.wait() {
                Ok(cortex_sandbox::tree::Exit::Code(code)) => WAITED + code,
                other => {
                    println!("the wait did not say how the command ended: {other:?}");
                    99
                }
            };
            tree.end();
            #[expect(clippy::exit, reason = "a child process ends with what it found")]
            std::process::exit(code);
        }
        let guard = (arguments[at] == HARNESS).then(guard);
        let _tree = Tree::spawn(command, guard.as_ref()).expect("start the tree");
        // A harness that is killed never gets to end its trees.
        loop {
            std::thread::park();
        }
    }
    run_if_child();
}

/// The guard: this binary, entered by the same test.
fn guard() -> Guard {
    Guard::new(
        std::env::current_exe().expect("the test binary has a path"),
        ["--exact", ENTRY, "--nocapture", GUARD],
    )
}

/// The command that starts a harness which starts `child` as its one tree.
fn harness(kind: &str, child: &ChildSpec) -> Command {
    harness_through(&[], kind, child)
}

/// The same, started through the program and arguments in `through`: a
/// shell that sets something up and then becomes the harness.
fn harness_through(through: &[&str], kind: &str, child: &ChildSpec) -> Command {
    let (program, arguments) = child.arguments(ENTRY);
    let own = std::env::current_exe().expect("the test binary has a path");
    let mut command = match through.split_first() {
        Some((first, rest)) => {
            let mut command = Command::new(first);
            command.args(rest).arg(own);
            command
        }
        None => Command::new(own),
    };
    command
        .args(["--exact", ENTRY, "--nocapture", kind, "--"])
        .arg(program)
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}
