//! The operating system's side of the terminal: one module per system, the
//! only places in this crate where `unsafe` is allowed, and what both share.

use std::sync::atomic::{AtomicU8, Ordering};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

/// What ended a wait for input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    /// Bytes arrived.
    Input,
    /// The window changed size.
    Resize,
    /// The program was stopped and has been continued: the terminal was
    /// put back while it was stopped, and is in raw mode again.
    Resumed,
    /// A [`Waker`](crate::Waker) woke the wait.
    Woken,
    /// The time ran out.
    Timeout,
}

/// Which keyboard reporting was turned on, so that putting the terminal
/// back turns it off: [`KEYBOARD`] and [`WIN32`] as bits.
static PROTOCOLS: AtomicU8 = AtomicU8::new(0);
/// The enhanced keyboard protocol.
const KEYBOARD: u8 = 1;
/// Windows Terminal's win32 input mode.
const WIN32: u8 = 2;

/// What puts the terminal's own state back after a panic, a signal or a
/// console event, when the presenter could not finish: ends a frame that was
/// cut short, resets the style, erases from the cursor's row down (the rest
/// of the live region, so a message printed next is not mixed into it),
/// turns off what was turned on, and turns automatic wrap back on. The raw-mode or console-mode half is the
/// system's. One for each set of keyboard reporting that may be on, indexed
/// by [`PROTOCOLS`].
const RESTORE: [&[u8]; 4] = [
    b"\x1b[?2026l\x1b[0m\r\x1b[J\x1b[?1004l\x1b[?2004l\x1b[?7h\x1b[?25h",
    b"\x1b[?2026l\x1b[0m\r\x1b[J\x1b[<u\x1b[?1004l\x1b[?2004l\x1b[?7h\x1b[?25h",
    b"\x1b[?2026l\x1b[0m\r\x1b[J\x1b[?9001l\x1b[?1004l\x1b[?2004l\x1b[?7h\x1b[?25h",
    b"\x1b[?2026l\x1b[0m\r\x1b[J\x1b[<u\x1b[?9001l\x1b[?1004l\x1b[?2004l\x1b[?7h\x1b[?25h",
];

/// The restore sequence for the terminal's state now. Async-signal-safe:
/// one atomic load.
pub(crate) fn restore_sequence() -> &'static [u8] {
    RESTORE[usize::from(PROTOCOLS.load(Ordering::SeqCst) & 3)]
}

fn set(bit: u8, on: bool) {
    if on {
        PROTOCOLS.fetch_or(bit, Ordering::SeqCst);
    } else {
        PROTOCOLS.fetch_and(!bit, Ordering::SeqCst);
    }
}

/// Records whether the enhanced keyboard protocol is on.
pub(crate) fn set_keyboard(on: bool) {
    set(KEYBOARD, on);
}

/// Records whether the win32 input mode is on.
pub(crate) fn set_win32(on: bool) {
    set(WIN32, on);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restore_sequence_turns_off_what_was_turned_on() {
        let has = |sequence: &[u8], part: &[u8]| sequence.windows(part.len()).any(|w| w == part);
        for (keyboard, win32) in [(false, false), (true, false), (false, true), (true, true)] {
            set_keyboard(keyboard);
            set_win32(win32);
            let sequence = restore_sequence();
            assert_eq!(has(sequence, b"\x1b[<u"), keyboard);
            assert_eq!(has(sequence, b"\x1b[?9001l"), win32);
            for needed in [
                &b"\x1b[?2026l"[..],
                b"\x1b[0m",
                b"\x1b[?1004l",
                b"\x1b[?2004l",
                b"\x1b[?7h",
                b"\x1b[?25h",
            ] {
                assert!(has(sequence, needed), "{needed:?}");
            }
        }
        set_keyboard(false);
        set_win32(false);
    }
}
