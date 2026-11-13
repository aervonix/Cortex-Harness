//! The demo in a pseudo-terminal (ConPTY on Windows): it draws, keys reach
//! it, and the terminal is put back after a normal exit, a panic, and a
//! signal or console event. Each test runs the shell child, which runs the
//! demo and then compares the terminal's modes with those it found.
//!
//! On Windows the shell turns virtual-terminal processing off first, as the
//! old console host has it, so these tests also show that the demo turns it
//! on: otherwise its escape sequences would be on the screen as text.

use std::time::{Duration, Instant};

use cortex_testkit::VTerm;
use cortex_testkit::pty::Session;

use super::demo::STATUS_MARK;
use super::{NOT_RESTORED, RESTORED, SHELL, STARTING, child_arguments, this_binary};

/// A bound on every wait, which only a broken test reaches.
const LIMIT: Duration = Duration::from_secs(20);

/// The coarse check on the first frame: five times the runner's budget for
/// the interface's first frame (`docs/performance-budgets.md`), for a demo
/// built without optimisation.
const FIRST_FRAME: Duration = if cfg!(windows) {
    Duration::from_millis(600)
} else {
    Duration::from_millis(250)
};

/// The coarse check on a key's echo: generous, as the budget's check in the
/// system tier is (`docs/performance-budgets.md`).
const KEY_ECHO: Duration = Duration::from_millis(250);

fn start(options: &str) -> Session {
    Session::start(&this_binary(), &child_arguments(SHELL, options), 100, 30).answering()
}

/// Waits for the demo's first frame and checks it came in time, with no
/// escape sequence shown as text.
fn first_frame(session: &mut Session) {
    let starting = session.wait_for(LIMIT, "the shell to start the demo", |t| t.shows(STARTING));
    let drawn = session.wait_for(LIMIT, "the first frame", |t| t.shows(STATUS_MARK));
    let took = drawn - starting;
    assert!(
        took < FIRST_FRAME,
        "the first frame took {took:?}, more than {FIRST_FRAME:?}"
    );
    let screen = session.screen().screen().join("\n");
    for literal in ["[?2026", "[?25", "[K", "[?2004", "\u{2190}["] {
        assert!(
            !screen.contains(literal),
            "an escape sequence is shown as text ({literal}):\n{screen}"
        );
    }
}

/// Waits for the shell's verdict and checks the terminal is as it was:
/// modes put back, nothing left on.
fn restored(session: &mut Session) {
    session.wait_for(LIMIT, "the shell's verdict", |t| {
        t.shows(RESTORED) || t.shows(NOT_RESTORED)
    });
    // The verdict may arrive in pieces: read up to its end.
    session.wait_for(LIMIT, "the whole verdict", |t| {
        t.shows(RESTORED) && t.shows(")") || t.shows("after:")
    });
    let screen = session.screen();
    assert!(
        screen.shows(RESTORED),
        "the modes were not put back:\n{}",
        screen.dump()
    );
    let modes = screen.modes();
    assert!(modes.cursor_visible, "the cursor is hidden");
    assert!(!modes.sync, "a synchronized update was left open");
    assert!(!modes.bracketed_paste, "bracketed paste was left on");
    // ConPTY asks the terminal for focus events itself, for the console it
    // runs, so on Windows the mode says nothing about the demo.
    assert!(cfg!(windows) || !modes.focus, "focus events were left on");
    assert!(
        screen.keyboard().is_empty(),
        "the keyboard protocol was left on"
    );
    session.wait_exit(LIMIT);
}

fn editor_shows(term: &VTerm, text: &str) -> bool {
    term.shows(&format!("> {text}"))
}

#[test]
fn sys_keys_are_drawn_and_the_terminal_is_put_back_after_a_normal_exit() {
    let mut session = start("rate=400");
    first_frame(&mut session);

    let mut worst = Duration::ZERO;
    let mut typed = String::new();
    for c in ['h', 'i', '!'] {
        typed.push(c);
        let sent = session.send(c.to_string().as_bytes());
        let shown = session.wait_for(LIMIT, "the key's echo", |t| editor_shows(t, &typed));
        worst = worst.max(shown.saturating_duration_since(sent));
    }
    assert!(
        worst < KEY_ECHO,
        "a key took {worst:?} to show, more than {KEY_ECHO:?}"
    );

    // A narrower window: the live region is drawn again at the new width.
    session.resize(60, 30);
    session.send(b"?");
    session.wait_for(LIMIT, "the echo after a resize", |t| {
        editor_shows(t, "hi!?")
    });
    let status_rows = session
        .screen()
        .screen()
        .iter()
        .filter(|row| row.contains(STATUS_MARK))
        .count();
    assert_eq!(status_rows, 1, "\n{}", session.screen().dump());

    session.send(b"\x03");
    restored(&mut session);
    // The live region is gone: the shell's prompt comes back where it was.
    assert!(
        !session.screen().shows(STATUS_MARK),
        "\n{}",
        session.screen().dump()
    );
}

#[test]
fn sys_the_terminal_is_put_back_after_a_panic() {
    let mut session = start("panic-on=P");
    first_frame(&mut session);
    let started = Instant::now();
    session.send(b"P");
    restored(&mut session);
    assert!(session.screen().shows("demo panic on request"));
    assert!(started.elapsed() < LIMIT);
}

#[test]
fn sys_the_terminal_is_put_back_after_a_signal_or_console_event() {
    let mut session = start("terminate-on=T");
    first_frame(&mut session);
    session.send(b"T");
    restored(&mut session);
}
