//! The integration tests of `cortex-term`: the presenter against the
//! virtual terminal (tier 2), and the demo in a pseudo-terminal (tier 3,
//! `sys_…`), on both systems.
//!
//! # Children
//!
//! The pseudo-terminal tests run this test binary again, as the demo or as a
//! small shell that runs the demo and checks the terminal after it. The
//! entry test [`child_entry`] becomes that child when its arguments say so,
//! and passes at once otherwise. No other program is built.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "tests: children are processes, the shell child waits for its own and reports on \
              its terminal, and a test fails by panicking"
)]

#[path = "../../examples/demo/app.rs"]
#[allow(
    dead_code,
    unreachable_pub,
    reason = "the demo is the example's, and this binary uses part of it"
)]
mod demo;
mod perf;
mod presenter;
mod system;

/// The name of the entry test.
const ENTRY: &str = "child_entry";
/// The argument that makes this binary the demo, with the demo's options
/// after it, separated by commas.
const DEMO: &str = "cxterm-demo=";
/// The argument that makes this binary a shell that runs the demo with the
/// options after it, and reports whether the terminal was put back.
const SHELL: &str = "cxterm-shell=";
/// What the shell prints when the terminal's modes after the demo are the
/// ones before it.
const RESTORED: &str = "restored: yes";
/// What it prints when they are not.
const NOT_RESTORED: &str = "restored: NO";
/// What it prints just before it starts the demo.
const STARTING: &str = "starting the demo";

/// The arguments that start this binary as a child.
fn child_arguments(mark: &str, options: &str) -> Vec<String> {
    vec![
        "--exact".to_owned(),
        ENTRY.to_owned(),
        "--nocapture".to_owned(),
        "--test-threads=1".to_owned(),
        format!("{mark}{options}"),
    ]
}

/// This test binary's path.
fn this_binary() -> String {
    std::env::current_exe()
        .expect("the test binary has a path")
        .to_str()
        .expect("the test binary's path is Unicode")
        .to_owned()
}

#[test]
fn child_entry() {
    let arguments: Vec<String> = std::env::args().collect();
    if let Some(options) = arguments.iter().find_map(|a| a.strip_prefix(DEMO)) {
        let options = demo::Options::parse(options.split(',')).expect("the demo's options");
        demo::run(&options).expect("the demo runs");
        #[expect(clippy::exit, reason = "a child ends when the demo does")]
        std::process::exit(0);
    }
    if let Some(options) = arguments.iter().find_map(|a| a.strip_prefix(SHELL)) {
        shell(options);
    }
}

/// Runs the demo as a shell would, and says whether it put the terminal's
/// modes back. On Windows it first turns virtual-terminal processing off,
/// as the old console host has it, so the demo must turn it on itself.
fn shell(options: &str) -> ! {
    use std::io::Write as _;

    cortex_term::pty::ignore_control_events();
    cortex_term::pty::set_vt_output(false).expect("turn off virtual-terminal processing");
    let before = cortex_term::pty::modes().expect("the terminal's modes");
    print!("{STARTING}\r\n");
    std::io::stdout().flush().expect("write to the terminal");
    let status = std::process::Command::new(this_binary())
        .args(child_arguments(DEMO, options))
        .status()
        .expect("start the demo");
    let after = cortex_term::pty::modes().expect("the terminal's modes");
    // Plain text, which needs no escape sequence: the console may not
    // process them now.
    if before == after {
        print!("\r\n{RESTORED} ({status})\r\n");
    } else {
        print!("\r\n{NOT_RESTORED}\r\nbefore: {before}\r\nafter:  {after}\r\n");
    }
    std::io::stdout().flush().expect("write to the terminal");
    #[expect(clippy::exit, reason = "a child ends when it has reported")]
    std::process::exit(0);
}
