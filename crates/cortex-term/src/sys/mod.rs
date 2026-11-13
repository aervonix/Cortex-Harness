//! The operating system's side of the terminal: one module per system, the
//! only places in this crate where `unsafe` is allowed, and what both share.

use std::sync::atomic::{AtomicBool, Ordering};

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

/// Whether the enhanced keyboard protocol was turned on, so that putting
/// the terminal back turns it off.
static KEYBOARD: AtomicBool = AtomicBool::new(false);

/// What puts the terminal's own state back after a panic, a signal or a
/// console event, when the presenter could not finish: ends a frame that was
/// cut short, resets the style, erases from the cursor's row down (the rest
/// of the live region, so a message printed next is not mixed into it), and
/// turns off what was turned on. The raw-mode or console-mode half is the
/// system's.
const RESTORE: &[u8] = b"\x1b[?2026l\x1b[0m\r\x1b[J\x1b[?1004l\x1b[?2004l\x1b[?25h";
/// The same, with the enhanced keyboard protocol turned off too.
const RESTORE_KEYBOARD: &[u8] = b"\x1b[?2026l\x1b[0m\r\x1b[J\x1b[<u\x1b[?1004l\x1b[?2004l\x1b[?25h";

/// The restore sequence for the terminal's state now. Async-signal-safe:
/// one atomic load.
pub(crate) fn restore_sequence() -> &'static [u8] {
    if KEYBOARD.load(Ordering::SeqCst) {
        RESTORE_KEYBOARD
    } else {
        RESTORE
    }
}

/// Records whether the enhanced keyboard protocol is on.
pub(crate) fn set_keyboard(on: bool) {
    KEYBOARD.store(on, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restore_sequence_turns_off_what_was_turned_on() {
        set_keyboard(false);
        let plain = restore_sequence();
        assert!(!plain.windows(4).any(|w| w == b"\x1b[<u"));
        set_keyboard(true);
        let with = restore_sequence();
        assert!(with.windows(4).any(|w| w == b"\x1b[<u"));
        set_keyboard(false);
        for sequence in [plain, with] {
            for needed in [
                &b"\x1b[?2026l"[..],
                b"\x1b[0m",
                b"\x1b[?1004l",
                b"\x1b[?2004l",
                b"\x1b[?25h",
            ] {
                assert!(
                    sequence.windows(needed.len()).any(|w| w == needed),
                    "{needed:?}"
                );
            }
        }
    }
}
