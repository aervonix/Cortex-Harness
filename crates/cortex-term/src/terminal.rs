//! The terminal itself: raw mode on the way in, put back on every way out,
//! and events on the way through.
//!
//! [`Terminal::enter`] turns raw mode on (on Windows: virtual-terminal
//! processing and input, and UTF-8 code pages), asks for bracketed paste and
//! focus events, and sends the capability queries. It never waits for a
//! reply. From then on [`Terminal::next`] gives the events, and the terminal
//! is the [`Sink`] the presenter writes frames to.
//!
//! # Every way out
//!
//! - **A normal exit:** [`Terminal::leave`], or dropping the terminal, turns
//!   off what was turned on and puts the modes back.
//! - **A panic:** a panic hook, installed once, does the same before the
//!   panic's message is printed, then calls the hook that was there before.
//! - **A signal** (SIGTERM, SIGHUP, SIGINT, SIGQUIT) **or a console event**
//!   (Ctrl+Break, the console closing, logging off, shutting down): a handler
//!   does the same, and then lets the signal or the event end the process as
//!   it would have. The handlers replace any installed before them. From then
//!   on nothing more is written: a frame would turn modes on in a terminal
//!   that is no longer the program's, so the next write fails.
//! - **Suspending** (SIGTSTP, or [`Terminal::suspend`] on Ctrl+Z): the
//!   terminal is put back while the program is stopped; when it continues,
//!   raw mode and everything that was turned on come back, with an
//!   [`Event::Resumed`].
//!
//! # The device
//!
//! What the operating system does is behind the small [`Device`] trait, so
//! that it can be replaced, and so that this layer's own logic (the escape
//! key's wait, the replies) is tested without a terminal.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use crate::caps::{Capabilities, Overrides, Platform};
use crate::inline::Sink;
use crate::input::{Decoder, Event, Reply};
use crate::sys::{self, Ready};

/// How long a lone ESC waits for the rest of a sequence before it is taken
/// as the Escape key. Long enough for a sequence split by a slow link, short
/// enough that Escape does not feel late.
pub const ESCAPE_WAIT: Duration = Duration::from_millis(50);

/// Turned on at the start, and again after a suspension: bracketed paste and
/// focus events, both harmless where they are not known; and automatic wrap
/// turned off, so that a character the terminal draws wider than it is
/// counted can never push a row onto the next and throw the presenter's
/// count of rows out.
const START: &[u8] = b"\x1b[?2004h\x1b[?1004h\x1b[?7l";
/// Turned off at the end, and automatic wrap turned back on. The enhanced
/// keyboard protocol and the win32 input mode, if they were turned on, are
/// turned off first.
const END: &[u8] = b"\x1b[0m\x1b[?1004l\x1b[?2004l\x1b[?7h\x1b[?25h";
/// How long leaving waits for the terminal's last reply to the capability
/// queries, when it has not come yet, so that the reply does not arrive at
/// the shell's prompt as typed text.
pub const REPLY_WAIT: Duration = Duration::from_millis(100);
/// Turns the enhanced keyboard protocol on, with the flag that tells apart
/// keys that are otherwise the same bytes.
const KEYBOARD_ON: &[u8] = b"\x1b[>1u";
const KEYBOARD_OFF: &[u8] = b"\x1b[<u";
/// Turns Windows Terminal's win32 input mode on, and off.
const WIN32_ON: &[u8] = b"\x1b[?9001h";
const WIN32_OFF: &[u8] = b"\x1b[?9001l";

/// Wakes a terminal that is waiting in [`Terminal::next`], from any thread.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    /// A waker that calls `wake`.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    /// Wakes the terminal: its [`next`](Terminal::next) returns `None`.
    pub fn wake(&self) {
        (self.0)();
    }
}

impl fmt::Debug for Waker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Waker")
    }
}

/// What the terminal layer needs from the operating system.
pub trait Device {
    /// The window's size: columns, then rows.
    ///
    /// # Errors
    ///
    /// When the system cannot say.
    fn size(&self) -> io::Result<(u16, u16)>;

    /// Writes `bytes` to the terminal, in one call where the system allows.
    ///
    /// # Errors
    ///
    /// When the terminal cannot be written to.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Waits for at most `timeout` (`None`: as long as it takes) for input,
    /// which it adds to `into`, or for something else to happen.
    ///
    /// # Errors
    ///
    /// When the terminal cannot be read, or has gone.
    fn wait(&mut self, timeout: Option<Duration>, into: &mut Vec<u8>) -> io::Result<Ready>;

    /// Something that wakes a [`wait`](Self::wait) from another thread.
    fn waker(&self) -> Waker;

    /// Stops the program as Ctrl+Z would, where the system can.
    fn suspend(&mut self);

    /// Puts the terminal's modes back. Called once, last.
    fn leave(&mut self);

    /// Whether the terminal is still the program's: `false` once it has
    /// been put back in an emergency (a panic, a signal), when nothing more
    /// should be read or written.
    fn active(&self) -> bool {
        true
    }

    /// The time now, on a clock that only goes forward. A device for tests
    /// gives a time of its own, so that waits are tested without waiting.
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The real terminal, through the operating system.
#[derive(Debug)]
pub struct System {
    _private: (),
}

impl Device for System {
    fn size(&self) -> io::Result<(u16, u16)> {
        sys::size()
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        sys::write(bytes)
    }

    fn wait(&mut self, timeout: Option<Duration>, into: &mut Vec<u8>) -> io::Result<Ready> {
        sys::wait(timeout, into)
    }

    fn waker(&self) -> Waker {
        Waker::new(sys::wake)
    }

    fn suspend(&mut self) {
        sys::suspend();
    }

    fn leave(&mut self) {
        sys::leave();
    }

    fn active(&self) -> bool {
        sys::active()
    }
}

/// Whether standard input and standard output are both a terminal, which
/// the interactive interface needs.
#[must_use]
pub fn is_terminal() -> bool {
    sys::is_terminal()
}

static PANIC_HOOK: Once = Once::new();

/// A terminal in raw mode. See the [module documentation](self).
#[derive(Debug)]
pub struct Terminal<D: Device = System> {
    device: D,
    caps: Capabilities,
    decoder: Decoder,
    events: VecDeque<Event>,
    bytes: Vec<u8>,
    keyboard: bool,
    win32: bool,
    active: bool,
    /// When the decoder began to keep bytes that more input could complete
    /// (a lone ESC): they are decoded as they stand once [`ESCAPE_WAIT`] has
    /// passed, however often the wait is cut short meanwhile.
    pending_since: Option<Instant>,
    /// Whether the reply that ends the capability queries has come.
    answered: bool,
}

impl Terminal<System> {
    /// Takes over the terminal on standard input and output, with its
    /// capabilities detected from the environment and `overrides` applied.
    ///
    /// # Errors
    ///
    /// When standard input or output is not a terminal, when the terminal is
    /// `TERM=dumb`, or when it cannot be put in raw mode. On Windows, also
    /// when the console cannot process escape sequences, as consoles before
    /// Windows 10 cannot.
    pub fn enter(overrides: Overrides) -> io::Result<Self> {
        let caps = Capabilities::detect(
            |name| std::env::var(name).ok(),
            Platform::current(),
            overrides,
        );
        if caps.dumb {
            return Err(io::Error::other(
                "this terminal (TERM=dumb) cannot show the interactive interface",
            ));
        }
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                sys::emergency();
                previous(info);
            }));
        });
        sys::enter()?;
        Self::with_device(System { _private: () }, caps)
    }
}

impl<D: Device> Terminal<D> {
    /// A terminal on `device`, already in raw mode, with `caps` as the
    /// starting capabilities. Writes the start sequences and the queries.
    ///
    /// # Errors
    ///
    /// When the device cannot be written to. The device is put back first.
    pub fn with_device(mut device: D, caps: Capabilities) -> io::Result<Self> {
        let mut start = START.to_vec();
        start.extend_from_slice(Capabilities::QUERIES);
        if let Err(error) = device.write(&start) {
            device.leave();
            return Err(error);
        }
        Ok(Self {
            device,
            caps,
            decoder: Decoder::new(),
            events: VecDeque::new(),
            bytes: Vec::new(),
            keyboard: false,
            win32: false,
            active: true,
            pending_since: None,
            answered: false,
        })
    }

    /// What the terminal can do, as known now. Replies to the queries
    /// update it as [`next`](Self::next) reads them.
    #[must_use]
    pub fn caps(&self) -> &Capabilities {
        &self.caps
    }

    /// The window's size: columns, then rows.
    ///
    /// # Errors
    ///
    /// When the system cannot say.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        self.device.size()
    }

    /// Something that wakes [`next`](Self::next) from another thread.
    #[must_use]
    pub fn waker(&self) -> Waker {
        self.device.waker()
    }

    /// The next event, waiting at most `timeout` for one (`None`: as long as
    /// it takes). `Ok(None)` when the time ran out or a [`Waker`] woke it.
    ///
    /// A reply to a capability query updates [`caps`](Self::caps) before it
    /// is returned. A reply that shows the enhanced keyboard protocol turns
    /// it on, unless a setting says not to.
    ///
    /// # Errors
    ///
    /// When the terminal cannot be read, or has gone.
    pub fn next(&mut self, timeout: Option<Duration>) -> io::Result<Option<Event>> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }
            let mut found = Vec::new();
            // What is kept is decoded as it stands once it has waited long
            // enough, whatever cut the earlier waits short.
            let left = self.pending_since.map(|since| {
                ESCAPE_WAIT.saturating_sub(self.device.now().saturating_duration_since(since))
            });
            if left == Some(Duration::ZERO) {
                self.decoder.flush(&mut found);
                self.pending_since = None;
                self.take(found)?;
                continue;
            }
            let wait = match (left, timeout) {
                (Some(left), Some(timeout)) => Some(left.min(timeout)),
                (Some(left), None) => Some(left),
                (None, timeout) => timeout,
            };
            self.bytes.clear();
            match self.device.wait(wait, &mut self.bytes)? {
                Ready::Input => {
                    self.decoder.feed(&self.bytes, &mut found);
                    // What is kept now waits its own time: from its arrival,
                    // not from that of something the same read finished.
                    if !self.decoder.pending() {
                        self.pending_since = None;
                    } else if self.pending_since.is_none() || !found.is_empty() {
                        self.pending_since = Some(self.device.now());
                    }
                }
                Ready::Resize => {
                    let (cols, rows) = self.device.size()?;
                    found.push(Event::Resize { cols, rows });
                }
                Ready::Resumed => {
                    self.resume()?;
                    found.push(Event::Resumed);
                }
                Ready::Woken => return Ok(None),
                // The escape's own wait: the next pass decodes it.
                Ready::Timeout if left.is_some() && wait == left => {}
                Ready::Timeout => return Ok(None),
            }
            self.take(found)?;
        }
    }

    /// Queues decoded events, taking in the replies first.
    fn take(&mut self, found: Vec<Event>) -> io::Result<()> {
        for event in found {
            if let Event::Reply(reply) = &event {
                self.reply(*reply)?;
            }
            self.events.push_back(event);
        }
        Ok(())
    }

    /// After a suspension, which turned everything off: turns back on what
    /// was on.
    fn resume(&mut self) -> io::Result<()> {
        let mut again = START.to_vec();
        if self.keyboard {
            again.extend_from_slice(KEYBOARD_ON);
        }
        if self.win32 {
            again.extend_from_slice(WIN32_ON);
        }
        self.device.write(&again)
    }

    fn reply(&mut self, reply: Reply) -> io::Result<()> {
        if reply == Reply::DeviceAttributes {
            self.answered = true;
        }
        self.caps.apply(&reply);
        if self.caps.keyboard && !self.keyboard {
            self.keyboard = true;
            sys::set_keyboard(true);
            self.device.write(KEYBOARD_ON)?;
        }
        if self.caps.win32_input && !self.win32 {
            self.win32 = true;
            sys::set_win32(true);
            self.device.write(WIN32_ON)?;
        }
        Ok(())
    }

    /// Stops the program as Ctrl+Z would (Unix only; nothing happens on
    /// Windows). The terminal is put back while it is stopped. When it
    /// continues, [`next`](Self::next) gives [`Event::Resumed`]: the shell
    /// may have written below the live region, so the presenter starts again
    /// where the cursor is ([`Inline::restart`](crate::Inline::restart)).
    pub fn suspend(&mut self) {
        self.device.suspend();
    }

    /// Turns off what was turned on and puts the terminal's modes back. The
    /// presenter should have [finished](crate::Inline::finish) first, so
    /// that the live region is gone.
    ///
    /// # Errors
    ///
    /// When the end sequences cannot be written. The modes are put back
    /// whatever happens.
    pub fn leave(mut self) -> io::Result<()> {
        self.end()
    }

    fn end(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        // After an emergency the terminal is in its ordinary mode already:
        // reading it now would take what the user typed for the shell.
        if self.device.active() {
            self.drain_replies();
        }
        let mut end = Vec::new();
        if self.keyboard {
            end.extend_from_slice(KEYBOARD_OFF);
        }
        if self.win32 {
            end.extend_from_slice(WIN32_OFF);
        }
        end.extend_from_slice(END);
        let written = self.device.write(&end);
        self.device.leave();
        if self.keyboard {
            sys::set_keyboard(false);
        }
        if self.win32 {
            sys::set_win32(false);
        }
        written
    }
}

impl<D: Device> Terminal<D> {
    /// Reads, and drops, input until the last reply to the capability
    /// queries has come, for at most [`REPLY_WAIT`]. Once the terminal is
    /// back in its ordinary mode, a late reply would reach the shell as
    /// typed text.
    fn drain_replies(&mut self) {
        let deadline = self.device.now() + REPLY_WAIT;
        let mut found = Vec::new();
        while !self.answered {
            let left = deadline.saturating_duration_since(self.device.now());
            if left.is_zero() {
                return;
            }
            self.bytes.clear();
            match self.device.wait(Some(left), &mut self.bytes) {
                Ok(Ready::Input) => {
                    found.clear();
                    self.decoder.feed(&self.bytes, &mut found);
                    self.answered |= found.contains(&Event::Reply(Reply::DeviceAttributes));
                }
                Ok(Ready::Timeout) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }
}

impl<D: Device> Sink for Terminal<D> {
    fn write_frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.device.write(bytes)
    }
}

impl<D: Device> Drop for Terminal<D> {
    fn drop(&mut self) {
        let _ = self.end();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::input::{Key, KeyCode, Mods};

    /// A device that plays back what a test gives it.
    #[derive(Debug, Default)]
    struct Script {
        steps: VecDeque<(Ready, Vec<u8>)>,
        written: Arc<Mutex<Vec<u8>>>,
        waits: Arc<Mutex<Vec<Option<Duration>>>>,
        left: Arc<Mutex<bool>>,
        /// The script's own clock: how far it is past `base`. A wait that
        /// times out moves it by the wait's length; any other step by
        /// [`STEP`].
        base: Option<Instant>,
        elapsed: Arc<Mutex<Duration>>,
    }

    /// How long a step of a script that is not a timeout takes.
    const STEP: Duration = Duration::from_millis(20);

    impl Device for Script {
        fn size(&self) -> io::Result<(u16, u16)> {
            Ok((100, 30))
        }
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.written.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn wait(&mut self, timeout: Option<Duration>, into: &mut Vec<u8>) -> io::Result<Ready> {
            self.waits.lock().unwrap().push(timeout);
            let (ready, bytes) = self
                .steps
                .pop_front()
                .unwrap_or((Ready::Timeout, Vec::new()));
            *self.elapsed.lock().unwrap() += match ready {
                Ready::Timeout => timeout.unwrap_or(Duration::ZERO),
                _ => STEP,
            };
            into.extend_from_slice(&bytes);
            Ok(ready)
        }
        fn now(&self) -> Instant {
            self.base.unwrap() + *self.elapsed.lock().unwrap()
        }
        fn waker(&self) -> Waker {
            Waker::new(|| {})
        }
        fn suspend(&mut self) {}
        fn leave(&mut self) {
            *self.left.lock().unwrap() = true;
        }
    }

    fn caps() -> Capabilities {
        Capabilities::detect(|_| None, Platform::Unix, Overrides::default())
    }

    fn script(steps: &[(Ready, &[u8])]) -> Script {
        Script {
            steps: steps.iter().map(|(r, b)| (*r, b.to_vec())).collect(),
            base: Some(Instant::now()),
            ..Script::default()
        }
    }

    #[test]
    fn it_starts_with_the_modes_and_the_queries_and_ends_by_turning_them_off() {
        let device = script(&[]);
        let (written, left) = (device.written.clone(), device.left.clone());
        let terminal = Terminal::with_device(device, caps()).unwrap();
        let start = written.lock().unwrap().clone();
        assert_eq!(start, [START, Capabilities::QUERIES].concat());
        terminal.leave().unwrap();
        assert!(*left.lock().unwrap());
        assert!(written.lock().unwrap().ends_with(END));
    }

    #[test]
    fn dropping_it_puts_the_terminal_back_too() {
        let device = script(&[]);
        let left = device.left.clone();
        drop(Terminal::with_device(device, caps()).unwrap());
        assert!(*left.lock().unwrap());
    }

    #[test]
    fn a_lone_escape_is_the_escape_key_after_a_short_wait() {
        let device = script(&[(Ready::Input, b"\x1b"), (Ready::Timeout, b"")]);
        let waits = device.waits.clone();
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        assert_eq!(
            terminal.next(None).unwrap(),
            Some(Event::Key(Key::plain(KeyCode::Esc)))
        );
        assert_eq!(*waits.lock().unwrap(), vec![None, Some(ESCAPE_WAIT)]);
    }

    #[test]
    fn the_rest_of_a_sequence_within_the_wait_is_one_key() {
        let device = script(&[(Ready::Input, b"\x1b"), (Ready::Input, b"[1;5A")]);
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        assert_eq!(
            terminal.next(None).unwrap(),
            Some(Event::Key(Key::with(KeyCode::Up, Mods::CTRL)))
        );
    }

    #[test]
    fn a_shorter_timeout_of_the_caller_returns_and_the_escape_still_comes() {
        let device = script(&[(Ready::Input, b"\x1b"), (Ready::Timeout, b"")]);
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        assert_eq!(terminal.next(Some(Duration::from_millis(5))).unwrap(), None);
        // A longer wait of the caller's is cut to what is left of the
        // escape's, and gives the escape.
        assert_eq!(
            terminal.next(Some(Duration::from_millis(100))).unwrap(),
            Some(Event::Key(Key::plain(KeyCode::Esc)))
        );
    }

    #[test]
    fn a_pending_escape_is_the_escape_key_however_often_the_wait_is_woken() {
        // While a stream runs, the loop is woken every few milliseconds. The
        // escape comes once 50 ms have passed since it arrived.
        let device = script(&[
            (Ready::Input, b"\x1b"),
            (Ready::Woken, b""),
            (Ready::Woken, b""),
            (Ready::Woken, b""),
        ]);
        let waits = device.waits.clone();
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        assert_eq!(
            terminal.next(Some(Duration::from_millis(16))).unwrap(),
            None
        );
        assert_eq!(
            terminal.next(Some(Duration::from_millis(16))).unwrap(),
            None
        );
        assert_eq!(
            terminal.next(Some(Duration::from_millis(16))).unwrap(),
            None,
            "40 ms after the escape"
        );
        assert_eq!(
            terminal.next(Some(Duration::from_millis(16))).unwrap(),
            Some(Event::Key(Key::plain(KeyCode::Esc))),
            "60 ms after the escape"
        );
        // Each wait was cut to what was left of the escape's.
        let waits = waits.lock().unwrap().clone();
        assert_eq!(waits[1], Some(Duration::from_millis(16)));
        assert_eq!(waits[3], Some(Duration::from_millis(10)));
    }

    #[test]
    fn after_a_suspension_what_was_on_is_turned_on_again() {
        let device = script(&[
            (Ready::Input, b"\x1b[?1u\x1b[?9001;2$y"),
            (Ready::Resumed, b""),
        ]);
        let written = device.written.clone();
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        terminal.next(None).unwrap();
        terminal.next(None).unwrap();
        written.lock().unwrap().clear();
        assert_eq!(terminal.next(None).unwrap(), Some(Event::Resumed));
        assert_eq!(
            *written.lock().unwrap(),
            [START, KEYBOARD_ON, WIN32_ON].concat()
        );
    }

    #[test]
    fn leaving_waits_a_little_for_the_last_reply() {
        let device = script(&[
            (Ready::Input, b"\x1b[?2026;2$y"),
            (Ready::Input, b"x\x1b[?62;22c"),
        ]);
        let (waits, written) = (device.waits.clone(), device.written.clone());
        let terminal = Terminal::with_device(device, caps()).unwrap();
        terminal.leave().unwrap();
        // Two reads, then no more: the reply had come.
        assert_eq!(waits.lock().unwrap().len(), 2);
        assert!(written.lock().unwrap().ends_with(END));
        // With no reply at all, it gives up after the bound.
        let device = script(&[]);
        let waits = device.waits.clone();
        Terminal::with_device(device, caps())
            .unwrap()
            .leave()
            .unwrap();
        assert_eq!(*waits.lock().unwrap(), [Some(REPLY_WAIT)]);
        // Once the reply has come, leaving reads nothing.
        let device = script(&[(Ready::Input, b"\x1b[?62c")]);
        let waits = device.waits.clone();
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        terminal.next(None).unwrap();
        terminal.leave().unwrap();
        assert_eq!(waits.lock().unwrap().len(), 1);
    }

    #[test]
    fn replies_update_the_capabilities_and_turn_on_the_keyboard_protocol() {
        let device = script(&[(Ready::Input, b"\x1b[?1u\x1b[?2026;4$y")]);
        let written = device.written.clone();
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        assert_eq!(
            terminal.next(None).unwrap(),
            Some(Event::Reply(Reply::Keyboard(1)))
        );
        assert!(terminal.caps().keyboard);
        assert!(written.lock().unwrap().ends_with(KEYBOARD_ON));
        assert_eq!(
            terminal.next(None).unwrap(),
            Some(Event::Reply(Reply::Mode {
                mode: 2026,
                state: 4
            }))
        );
        assert!(!terminal.caps().sync);
        terminal.leave().unwrap();
        let all = written.lock().unwrap().clone();
        assert!(all.ends_with(&[KEYBOARD_OFF, END].concat()));
        assert!(
            !sys::restore_sequence()
                .windows(4)
                .any(|w| w == KEYBOARD_OFF)
        );
    }

    #[test]
    fn a_terminal_that_has_the_win32_input_mode_gets_it_until_the_end() {
        let device = script(&[(Ready::Input, b"\x1b[?9001;2$y\x1b[13;28;13;1;16;1_")]);
        let written = device.written.clone();
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        assert_eq!(
            terminal.next(None).unwrap(),
            Some(Event::Reply(Reply::Mode {
                mode: 9001,
                state: 2
            }))
        );
        assert!(written.lock().unwrap().ends_with(WIN32_ON));
        assert_eq!(
            terminal.next(None).unwrap(),
            Some(Event::Key(Key::with(KeyCode::Enter, Mods::SHIFT)))
        );
        terminal.leave().unwrap();
        assert!(
            written
                .lock()
                .unwrap()
                .ends_with(&[WIN32_OFF, END].concat())
        );
    }

    #[test]
    fn a_setting_keeps_the_keyboard_protocol_off() {
        let overrides = Overrides {
            keyboard: Some(false),
            ..Overrides::default()
        };
        let caps = Capabilities::detect(|_| None, Platform::Unix, overrides);
        let device = script(&[(Ready::Input, b"\x1b[?1u\x1b[?9001;2$y")]);
        let written = device.written.clone();
        let mut terminal = Terminal::with_device(device, caps).unwrap();
        terminal.next(None).unwrap();
        terminal.next(None).unwrap();
        assert!(!written.lock().unwrap().windows(5).any(|w| w == KEYBOARD_ON));
        assert!(!written.lock().unwrap().windows(8).any(|w| w == WIN32_ON));
    }

    #[test]
    fn resizes_resumptions_and_wakes() {
        let device = script(&[
            (Ready::Resize, b""),
            (Ready::Resumed, b""),
            (Ready::Woken, b""),
        ]);
        let mut terminal = Terminal::with_device(device, caps()).unwrap();
        assert_eq!(
            terminal.next(None).unwrap(),
            Some(Event::Resize {
                cols: 100,
                rows: 30
            })
        );
        assert_eq!(terminal.next(None).unwrap(), Some(Event::Resumed));
        assert_eq!(terminal.next(None).unwrap(), None);
    }
}
