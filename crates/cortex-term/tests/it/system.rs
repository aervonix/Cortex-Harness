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
use super::{DEMO, NOT_RESTORED, RESTORED, SHELL, STARTING, child_arguments, this_binary};

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
    // ConPTY asks the terminal for the win32 input mode itself, and draws
    // with automatic wrap as it likes, so on Windows these say nothing about
    // the demo.
    if cfg!(unix) {
        assert!(!modes.win32_input, "the win32 input mode was left on");
        assert!(modes.autowrap, "automatic wrap was left off");
    }
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
    // The rule under the text is as wide as a line may be at the new width
    // (59 columns), which only a frame drawn after the resize was seen has.
    session.wait_for(LIMIT, "the live region drawn at the new width", |t| {
        t.screen().iter().any(|row| {
            row.chars().count() == 59 && row.chars().all(|c| c == '-' || c == '\u{2500}')
        })
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
    // The terminal was put back before the panic's message was printed:
    // each of its lines starts at the left edge, where in raw mode a line
    // break would not have returned the cursor there.
    let screen = session.screen().screen();
    assert!(
        screen
            .iter()
            .any(|row| row.starts_with("demo panic on request")),
        "the panic's message was printed in raw mode:\n{}",
        session.screen().dump()
    );
    assert!(started.elapsed() < LIMIT);
}

#[test]
fn sys_the_terminal_is_put_back_after_a_signal_or_console_event() {
    let mut session = start("terminate-on=T");
    first_frame(&mut session);
    session.send(b"T");
    restored(&mut session);
}

/// Waits until the status line names `key` as the last key read.
fn last_key(session: &mut Session, key: &str) {
    let want = format!("last key {key}");
    session.wait_for(LIMIT, &want.clone(), |t| {
        t.screen().iter().any(|row| row.trim_end().ends_with(&want))
    });
}

/// `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`: one key as Windows Terminal's win32
/// input mode reports it, pressed and released.
fn record(vk: u32, unit: u32, state: u32) -> Vec<u8> {
    let mut bytes = format!("\x1b[{vk};0;{unit};1;{state};1_").into_bytes();
    bytes.extend(format!("\x1b[{vk};0;{unit};0;{state};1_").into_bytes());
    bytes
}

#[test]
fn sys_keys_arrive_with_their_modifiers() {
    let mut session =
        Session::start(&this_binary(), &child_arguments(DEMO, "rate=400"), 100, 30).answering();
    session.wait_for(LIMIT, "the first frame", |t| t.shows(STATUS_MARK));

    // A lone Escape is the Escape key once nothing follows it.
    session.send(b"\x1b");
    last_key(&mut session, "Esc");
    // With a key right after it, Alt with that key; and a sequence with
    // modifiers. Windows Terminal never sends these to a ConPTY, which
    // asks it for win32 input records (below) and misreads plain
    // sequences that follow an Escape: there they are checked by
    // sequence only, in the decoder's unit tests.
    if cfg!(unix) {
        session.send(b"\x1bx");
        last_key(&mut session, "Alt+x");
        session.send(b"\x1b[1;5A");
        last_key(&mut session, "Ctrl+Up");
        // The demo turns the win32 input mode on when the terminal says it
        // has it, as this driver does, playing Windows Terminal.
        session.wait_for(LIMIT, "the win32 input mode", |t| t.modes().win32_input);
    }

    // Keys as Windows Terminal sends them: win32 input records. Each is told
    // apart from the key plain bytes would merge it with. On Windows they
    // go through ConPTY to the demo, which asked for the mode itself.
    const SHIFT: u32 = 0x10;
    const LEFT_CTRL: u32 = 0x08;
    const LEFT_ALT: u32 = 0x02;
    const ENHANCED: u32 = 0x100;
    for (bytes, name) in [
        (record(0x0d, 13, SHIFT), "Shift+Enter"),
        (record(0x0d, 13, LEFT_CTRL), "Ctrl+Enter"),
        (record(0x09, 9, LEFT_CTRL), "Ctrl+Tab"),
        (record(0x41, 1, LEFT_CTRL | SHIFT), "Ctrl+Shift+a"),
        (record(0x58, u32::from('x'), LEFT_ALT), "Alt+x"),
        (record(0x20, 0, LEFT_CTRL), "Ctrl+Space"),
        (record(0x26, 0, LEFT_CTRL | ENHANCED), "Ctrl+Up"),
        (record(0x1b, 27, 0), "Esc"),
    ] {
        session.send(&bytes);
        last_key(&mut session, name);
    }
    session.send(b"\x03");
    session.wait_exit(LIMIT);
}

/// Whether `haystack` holds `needle`.
#[cfg(unix)]
fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Ctrl+Z: the demo puts the terminal back and stops itself; continued, it
/// turns everything on again and draws.
///
/// The demo here leads its own session in the pseudo-terminal, so its
/// process group has no parent in that session: Linux does not stop such a
/// group, and the demo goes straight on as if continued at once. What is
/// checked is what the demo writes on the way in and out of the stop, in
/// order. The stop itself is not reached by any test
/// (outstanding/terminal-test-gaps.md).
#[cfg(unix)]
#[test]
fn sys_after_ctrl_z_and_fg_everything_is_on_again() {
    let mut session =
        Session::start(&this_binary(), &child_arguments(DEMO, "rate=400"), 100, 30).answering();
    session.wait_for(LIMIT, "the first frame", |t| {
        t.shows(STATUS_MARK) && t.modes().bracketed_paste && t.modes().win32_input
    });
    let mark: usize = session.recording().iter().map(|c| c.bytes.len()).sum();
    session.send(b"\x1a");
    // As a shell's `fg` would, in case the system did stop it.
    cortex_term::pty::continue_process(session.pid()).unwrap();
    let since = |session: &Session| -> Vec<u8> {
        let all: Vec<u8> = session
            .recording()
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect();
        all[mark..].to_vec()
    };
    let deadline = Instant::now() + LIMIT;
    loop {
        let bytes = since(&session);
        // Put back (paste, the win32 input mode off, wrap on), then all of
        // it on again and a frame.
        if let Some(back) = bytes
            .windows(8)
            .position(|w| w == b"\x1b[?2004l")
            .filter(|&at| holds(&bytes[..at], b"\x1b[?9001l") && holds(&bytes, b"\x1b[?7h"))
            && holds(&bytes[back..], b"\x1b[?2004h\x1b[?1004h\x1b[?7l")
            && holds(&bytes[back..], b"\x1b[?9001h")
            && session.screen().shows(STATUS_MARK)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the demo did not put the terminal back and turn it on again: {:?}",
            String::from_utf8_lossy(&bytes)
        );
        session.step(Duration::from_millis(50));
    }
    let modes = session.screen().modes();
    assert!(modes.bracketed_paste && modes.focus && modes.win32_input && !modes.autowrap);
    session.send(b"k");
    session.wait_for(LIMIT, "a key after resuming", |t| editor_shows(t, "k"));
    session.send(b"\x03");
    session.wait_exit(LIMIT);
}
