//! Input decoding: the bytes a terminal sends, as keys, pastes, focus
//! changes and replies to queries.
//!
//! One decoder serves both systems. On Windows the terminal layer turns on
//! the console's virtual-terminal input, so a Windows terminal sends the
//! same sequences as a Linux one (`docs/architecture/09-tui.md`, "Windows").
//!
//! # Bytes arrive in pieces
//!
//! A read can end anywhere: inside a UTF-8 character, inside an escape
//! sequence, inside a paste. The [`Decoder`] keeps what it cannot decode yet
//! and waits for more, so the same bytes decode the same way however they are
//! split. One thing cannot be told from the bytes alone: a lone ESC is either
//! the Escape key or the start of a sequence whose rest is still on its way.
//! The decoder keeps it, says so with [`Decoder::pending`], and the reader
//! calls [`Decoder::flush`] when nothing more arrives within a short time:
//! then it is the Escape key.
//!
//! # Windows Terminal's win32 input mode
//!
//! Virtual-terminal input cannot tell some keys apart: `Shift+Enter` is the
//! same byte as `Enter`, `Ctrl+Tab` as `Tab`, `Ctrl+Shift+A` as `Ctrl+A`.
//! Windows Terminal can instead report every key as a console input record
//! in a sequence of its own, `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _` (the virtual
//! key, the scan code, the character as a UTF-16 unit, down or up, the
//! modifier state, the repeat count). The terminal layer turns that on where
//! the terminal says it has it, and this decoder reads those sequences beside
//! the others: a key with all its modifiers, a release dropped, the two
//! halves of a character outside the Basic Multilingual Plane joined, and the
//! modifiers that only produced a character (Shift for `A`, AltGr for `@`)
//! not reported as modifiers.
//!
//! # What is left out
//!
//! - **Key releases**, which a terminal reports only under the enhanced
//!   keyboard protocol when asked for them, are dropped.
//! - **Sequences the decoder does not know** are dropped whole. None of their
//!   bytes becomes a key, so an unknown sequence never types text.

/// A key with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    /// Which key.
    pub code: KeyCode,
    /// The modifiers held with it.
    pub mods: Mods,
}

impl Key {
    /// A key with no modifier.
    #[must_use]
    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            mods: Mods::NONE,
        }
    }

    /// A key with modifiers.
    #[must_use]
    pub const fn with(code: KeyCode, mods: Mods) -> Self {
        Self { code, mods }
    }
}

/// Which key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A character. With Ctrl held, the letter itself: Ctrl+A is
    /// `Char('a')` with [`Mods::CTRL`].
    Char(char),
    /// Enter or Return.
    Enter,
    /// Tab. Shift+Tab is `Tab` with [`Mods::SHIFT`].
    Tab,
    /// Backspace.
    Backspace,
    /// Escape.
    Esc,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Insert.
    Insert,
    /// Delete.
    Delete,
    /// A function key, F1 to F24.
    F(u8),
}

/// Modifier keys, as a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods(u8);

impl Mods {
    /// No modifier.
    pub const NONE: Self = Self(0);
    /// Shift.
    pub const SHIFT: Self = Self(1);
    /// Alt, or Option.
    pub const ALT: Self = Self(2);
    /// Control.
    pub const CTRL: Self = Self(4);
    /// The Windows, Command or Super key.
    pub const SUPER: Self = Self(8);

    /// Whether every modifier of `other` is held.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets together.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether no modifier is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The modifiers of an xterm-style parameter: 1 + the sum of the bits.
    fn from_param(param: u32) -> Self {
        let bits = u8::try_from(param.saturating_sub(1) & 0x0f).unwrap_or(0);
        Self(bits)
    }
}

impl std::ops::BitOr for Mods {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

/// A terminal's answer to one of the capability queries
/// ([`Capabilities::QUERIES`](crate::Capabilities::QUERIES)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reply {
    /// The state of a private mode (`CSI ? mode ; state $ y`): 0 not known,
    /// 1 set, 2 reset, 3 always set, 4 always reset.
    Mode {
        /// The mode asked about.
        mode: u32,
        /// Its state.
        state: u32,
    },
    /// The enhanced keyboard protocol's current flags (`CSI ? flags u`): the
    /// terminal has the protocol.
    Keyboard(u32),
    /// The background colour, as red, green and blue.
    Background(u8, u8, u8),
    /// The primary device attributes (`CSI ? … c`). Every terminal answers
    /// this, so it marks the end of the replies to a batch of queries.
    DeviceAttributes,
}

/// Something the terminal sent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Event {
    /// A key was pressed, or repeated.
    Key(Key),
    /// Text was pasted, with line breaks as `\n`. Control characters are
    /// kept: whoever shows it sanitises it like any other content. A paste
    /// of more than 4 MiB arrives as several of these, one after another,
    /// with nothing between them: append them until something else comes.
    Paste(String),
    /// The terminal window gained (`true`) or lost focus.
    Focus(bool),
    /// An answer to a capability query.
    Reply(Reply),
    /// The window changed size. Not from the decoder: the
    /// [`Terminal`](crate::Terminal) reports it.
    Resize {
        /// The new width, in columns.
        cols: u16,
        /// The new height, in rows.
        rows: u16,
    },
    /// The program was stopped and has been continued. Not from the
    /// decoder: the [`Terminal`](crate::Terminal) reports it.
    Resumed,
}

/// The longest escape sequence kept while waiting for its end. A longer one
/// is not a sequence any terminal sends, and is dropped.
const LONGEST_SEQUENCE: usize = 256;

/// The end of a bracketed paste.
const PASTE_END: &[u8] = b"\x1b[201~";

/// Turns the bytes a terminal sends into [`Event`]s.
#[derive(Debug, Default)]
pub struct Decoder {
    /// Bytes not decoded yet: the beginning of something whose end has not
    /// arrived.
    buffer: Vec<u8>,
    /// Inside a bracketed paste: what has been pasted so far.
    paste: Option<Vec<u8>>,
    /// The first half of a character outside the Basic Multilingual Plane,
    /// from a win32 input record, waiting for its second half.
    high_surrogate: Option<u32>,
}

/// One key as Windows Terminal's win32 input mode reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    /// The virtual-key code.
    vk: u32,
    /// The character, as one UTF-16 unit; 0 for none.
    unit: u32,
    /// Pressed (`true`) or released.
    down: bool,
    /// The modifier and lock state.
    state: u32,
}

/// The bits of a record's modifier state.
const RIGHT_ALT: u32 = 0x01;
const LEFT_ALT: u32 = 0x02;
const RIGHT_CTRL: u32 = 0x04;
const LEFT_CTRL: u32 = 0x08;
const SHIFT: u32 = 0x10;

/// What decoding one item from the front of the buffer gave.
enum Step {
    /// This many bytes were used, and gave this event, or nothing.
    Done(usize, Option<Event>),
    /// This many bytes were used, and were a win32 input record.
    Record(usize, Record),
    /// The bytes are the beginning of something longer.
    Incomplete,
}

impl Decoder {
    /// A decoder that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes `bytes`, which follow whatever came before, and adds what
    /// they complete to `events`.
    pub fn feed(&mut self, bytes: &[u8], events: &mut Vec<Event>) {
        self.buffer.extend_from_slice(bytes);
        self.decode(events, false);
    }

    /// Whether bytes are kept that more input would complete: the reader
    /// waits a short time and then calls [`flush`](Self::flush).
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.buffer.is_empty() && self.paste.is_none()
    }

    /// Nothing more arrived: decodes what was kept as it stands. A lone ESC
    /// is the Escape key, and ESC followed by the start of a sequence that
    /// never finished is Alt with the next character. A paste is never cut
    /// short by this: it lasts until its end arrives.
    pub fn flush(&mut self, events: &mut Vec<Event>) {
        if self.paste.is_none() {
            self.decode(events, true);
        }
    }

    fn decode(&mut self, events: &mut Vec<Event>, flushing: bool) {
        let mut start = 0;
        loop {
            if let Some(paste) = &mut self.paste {
                let rest = &self.buffer[start..];
                if let Some(end) = find(rest, PASTE_END) {
                    paste.extend_from_slice(&rest[..end]);
                    start += end + PASTE_END.len();
                    let text = paste_text(paste);
                    self.paste = None;
                    events.push(Event::Paste(text));
                    continue;
                }
                // Keep any tail that could be the start of the end marker.
                let keep = (1..PASTE_END.len())
                    .rev()
                    .find(|&n| rest.len() >= n && rest.ends_with(&PASTE_END[..n]))
                    .unwrap_or(0);
                paste.extend_from_slice(&rest[..rest.len() - keep]);
                start = self.buffer.len() - keep;
                if paste.len() >= PASTE_PIECE {
                    events.push(Event::Paste(paste_piece(paste)));
                }
                break;
            }
            if start >= self.buffer.len() {
                break;
            }
            match step(&self.buffer[start..], flushing) {
                Step::Done(used, event) => {
                    start += used;
                    match event {
                        Some(Event::Paste(_)) => self.paste = Some(Vec::new()),
                        Some(event) => events.push(event),
                        None => {}
                    }
                }
                Step::Record(used, record) => {
                    start += used;
                    if let Some(event) = self.record(record) {
                        events.push(event);
                    }
                }
                Step::Incomplete => break,
            }
        }
        self.buffer.drain(..start);
    }

    /// The key a win32 input record stands for: `None` for a release, for a
    /// key that is only a modifier, and for the first half of a character.
    fn record(&mut self, record: Record) -> Option<Event> {
        if !record.down {
            return None;
        }
        let code = match record.vk {
            0x08 => KeyCode::Backspace,
            0x09 => KeyCode::Tab,
            0x0d => KeyCode::Enter,
            0x1b => KeyCode::Esc,
            0x21 => KeyCode::PageUp,
            0x22 => KeyCode::PageDown,
            0x23 => KeyCode::End,
            0x24 => KeyCode::Home,
            0x25 => KeyCode::Left,
            0x26 => KeyCode::Up,
            0x27 => KeyCode::Right,
            0x28 => KeyCode::Down,
            0x2d => KeyCode::Insert,
            0x2e => KeyCode::Delete,
            0x70..=0x87 => KeyCode::F(u8::try_from(record.vk - 0x6f).ok()?),
            // Shift, Ctrl, Alt, Caps Lock, the Windows keys, Num Lock,
            // Scroll Lock, and the left and right halves of the first three.
            0x10..=0x12 | 0x14 | 0x5b | 0x5c | 0x90 | 0x91 | 0xa0..=0xa5 => return None,
            _ => return self.record_char(record),
        };
        Some(Event::Key(Key::with(code, record_mods(record.state))))
    }

    /// A record of a key that types a character, or of Ctrl with one.
    fn record_char(&mut self, record: Record) -> Option<Event> {
        let unit = record.unit;
        let typed = match unit {
            0xd800..=0xdbff => {
                self.high_surrogate = Some(unit);
                return None;
            }
            0xdc00..=0xdfff => {
                let high = self.high_surrogate.take()?;
                char::from_u32(0x10000 + ((high - 0xd800) << 10) + (unit - 0xdc00))
            }
            _ => char::from_u32(unit),
        }
        .filter(|c| !c.is_control() && *c != '\0');
        let mut state = record.state;
        let c = if let Some(c) = typed {
            // AltGr is Right Alt with Left Ctrl: it made the character.
            if state & RIGHT_ALT != 0 && state & LEFT_CTRL != 0 {
                state &= !(RIGHT_ALT | LEFT_CTRL);
            }
            // Shift alone is in the character itself.
            if state & (RIGHT_ALT | LEFT_ALT | RIGHT_CTRL | LEFT_CTRL) == 0 {
                state &= !SHIFT;
            }
            c
        } else {
            // Ctrl with a letter or a digit types a control character or
            // none: the key itself says which.
            match record.vk {
                0x41..=0x5a => char::from_u32(record.vk + 0x20)?,
                0x30..=0x39 | 0x20 => char::from_u32(record.vk)?,
                _ => return None,
            }
        };
        Some(Event::Key(Key::with(KeyCode::Char(c), record_mods(state))))
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// How much of a paste is kept before it is handed on: a paste larger than
/// this arrives as several [`Event::Paste`]s, in order, so that a paste
/// whose end never comes cannot take all the memory there is.
const PASTE_PIECE: usize = 1 << 22;

/// The front of a long paste as text, leaving in `paste` what may be the
/// first half of a character or of a line break.
fn paste_piece(paste: &mut Vec<u8>) -> String {
    // A character cut at the end starts in the last three bytes.
    let mut cut = paste.len();
    for back in 1..=3.min(paste.len()) {
        let index = paste.len() - back;
        let byte = paste[index];
        if byte & 0xc0 == 0x80 {
            continue;
        }
        let length = match byte {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        if length > back {
            cut = index;
        }
        break;
    }
    if cut > 0 && paste[cut - 1] == b'\r' {
        cut -= 1;
    }
    let rest = paste.split_off(cut);
    let piece = paste_text(paste);
    *paste = rest;
    piece
}

/// Pasted bytes as text, with each line break as `\n`.
fn paste_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Decodes one item from the front of `bytes`, which is not empty.
fn step(bytes: &[u8], flushing: bool) -> Step {
    match bytes[0] {
        0x1b => escape(bytes, flushing),
        byte @ (0..0x20 | 0x7f) => Step::Done(1, Some(Event::Key(control(byte)))),
        0x20..0x7f => Step::Done(1, Some(key(KeyCode::Char(char::from(bytes[0]))))),
        _ => match utf8(bytes) {
            Some((c, used)) => Step::Done(used, Some(key(KeyCode::Char(c)))),
            None if flushing => Step::Done(bytes.len(), Some(key(KeyCode::Char('\u{fffd}')))),
            None => Step::Incomplete,
        },
    }
}

/// The modifiers of a record's state.
fn record_mods(state: u32) -> Mods {
    let mut mods = Mods::NONE;
    if state & SHIFT != 0 {
        mods = mods | Mods::SHIFT;
    }
    if state & (LEFT_ALT | RIGHT_ALT) != 0 {
        mods = mods | Mods::ALT;
    }
    if state & (LEFT_CTRL | RIGHT_CTRL) != 0 {
        mods = mods | Mods::CTRL;
    }
    mods
}

/// A win32 input record (`CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`) at the front of
/// `bytes`, with the bytes it takes; `None` when `bytes` do not start with a
/// whole one.
fn win32_record(bytes: &[u8]) -> Option<(usize, Record)> {
    let body = bytes.get(2..)?;
    let end = body.iter().position(|byte| (0x40..=0x7e).contains(byte))?;
    if body[end] != b'_' || !body[..end].iter().all(|b| b.is_ascii_digit() || *b == b';') {
        return None;
    }
    let groups = params(&body[..end]);
    let number = |index: usize, default: u32| -> u32 {
        groups
            .get(index)
            .and_then(|group| group.first())
            .copied()
            .flatten()
            .unwrap_or(default)
    };
    Some((
        2 + end + 1,
        Record {
            vk: number(0, 0),
            unit: number(2, 0),
            down: number(3, 0) == 1,
            state: number(4, 0),
        },
    ))
}

fn key(code: KeyCode) -> Event {
    Event::Key(Key::plain(code))
}

/// The key a control byte stands for.
fn control(byte: u8) -> Key {
    match byte {
        b'\r' => Key::plain(KeyCode::Enter),
        b'\t' => Key::plain(KeyCode::Tab),
        0x7f => Key::plain(KeyCode::Backspace),
        // Ctrl+Backspace in most terminals, and Ctrl+H in the rest.
        0x08 => Key::with(KeyCode::Backspace, Mods::CTRL),
        0 => Key::with(KeyCode::Char(' '), Mods::CTRL),
        0x1b => Key::plain(KeyCode::Esc),
        // Ctrl+A to Ctrl+Z, Ctrl+J (line feed) among them.
        0x01..=0x1a => Key::with(KeyCode::Char(char::from(b'a' + byte - 1)), Mods::CTRL),
        // Ctrl+\, Ctrl+], Ctrl+^ and Ctrl+_.
        _ => Key::with(KeyCode::Char(char::from(byte + 0x40)), Mods::CTRL),
    }
}

/// The first character of `bytes`, which starts with a byte above ASCII,
/// and its length; `None` when it is not complete yet. A byte that cannot
/// start a character, or a character that is not valid, is one U+FFFD.
fn utf8(bytes: &[u8]) -> Option<(char, usize)> {
    let length = match bytes[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Some(('\u{fffd}', 1)),
    };
    // Every byte after the first must be a continuation byte. A wrong one is
    // known before the character is complete.
    for (index, byte) in bytes.iter().enumerate().take(length).skip(1) {
        if byte & 0xc0 != 0x80 {
            return Some(('\u{fffd}', index));
        }
    }
    if bytes.len() < length {
        return None;
    }
    match std::str::from_utf8(&bytes[..length]) {
        Ok(text) => text.chars().next().map(|c| (c, length)),
        Err(_) => Some(('\u{fffd}', length)),
    }
}

/// Decodes what starts with ESC.
fn escape(bytes: &[u8], flushing: bool) -> Step {
    let Some(&second) = bytes.get(1) else {
        return if flushing {
            Step::Done(1, Some(key(KeyCode::Esc)))
        } else {
            Step::Incomplete
        };
    };
    let sequence = match second {
        b'[' => {
            if let Some((used, record)) = win32_record(bytes) {
                return Step::Record(used, record);
            }
            csi(bytes)
        }
        b'O' => ss3(bytes),
        b']' => string(bytes, 2).map(|(used, body)| (used, osc(body))),
        b'P' | b'_' | b'^' | b'X' => string(bytes, 2).map(|(used, _)| (used, None)),
        _ => {
            // Alt with the key that follows: ESC then a character or a
            // control byte. ESC ESC is Alt+Escape.
            return match step(&bytes[1..], flushing) {
                Step::Done(used, Some(Event::Key(key))) => Step::Done(
                    1 + used,
                    Some(Event::Key(Key::with(key.code, key.mods | Mods::ALT))),
                ),
                Step::Done(used, other) => Step::Done(1 + used, other),
                Step::Record(used, record) => Step::Record(1 + used, record),
                Step::Incomplete => Step::Incomplete,
            };
        }
    };
    match sequence {
        Some((used, event)) => Step::Done(used, event),
        None if bytes.len() > LONGEST_SEQUENCE => {
            // Not a sequence any terminal sends. It is not kept growing:
            // what has arrived of it, up to the limit, is dropped.
            Step::Done(LONGEST_SEQUENCE, None)
        }
        None if flushing => {
            // It never finished: Alt with the introducer, and the rest is
            // decoded on its own.
            Step::Done(
                2,
                Some(Event::Key(Key::with(
                    KeyCode::Char(char::from(second)),
                    Mods::ALT,
                ))),
            )
        }
        None => Step::Incomplete,
    }
}

/// A string sequence (OSC, DCS and their kind) starting at `from`, ended by
/// BEL or by ESC `\`: the bytes used and the body; `None` until its end has
/// arrived.
fn string(bytes: &[u8], from: usize) -> Option<(usize, &[u8])> {
    let body = &bytes[from..];
    for (index, &byte) in body.iter().enumerate() {
        match byte {
            0x07 => return Some((from + index + 1, &body[..index])),
            0x1b if body.get(index + 1) == Some(&b'\\') => {
                return Some((from + index + 2, &body[..index]));
            }
            _ => {}
        }
    }
    None
}

/// An OSC reply: only the background colour is asked for.
fn osc(body: &[u8]) -> Option<Event> {
    let text = std::str::from_utf8(body).ok()?;
    let spec = text.strip_prefix("11;")?;
    let (r, g, b) = parse_rgb(spec)?;
    Some(Event::Reply(Reply::Background(r, g, b)))
}

/// `rgb:RRRR/GGGG/BBBB`, with one to four hexadecimal digits a channel.
fn parse_rgb(spec: &str) -> Option<(u8, u8, u8)> {
    let channels = spec.strip_prefix("rgb:")?;
    let mut parts = channels.split('/');
    let mut channel = || -> Option<u8> {
        let digits = parts.next()?;
        if digits.is_empty() || digits.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        let max = (1u32 << (4 * digits.len())) - 1;
        u8::try_from(value * 255 / max).ok()
    };
    let rgb = (channel()?, channel()?, channel()?);
    parts.next().is_none().then_some(rgb)
}

/// SS3: ESC O and one byte.
fn ss3(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    let &last = bytes.get(2)?;
    let code = match last {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'M' => KeyCode::Enter,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return Some((3, None)),
    };
    Some((3, Some(key(code))))
}

/// A control sequence: ESC [, parameters, intermediates, a final byte.
fn csi(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    let body = &bytes[2..];
    let mut end = None;
    for (index, &byte) in body.iter().enumerate() {
        match byte {
            0x40..=0x7e => {
                end = Some(index);
                break;
            }
            0x20..=0x3f => {}
            // A byte no sequence holds: this was never one, but `Alt+[`
            // followed by what was typed next, which is decoded on its own.
            _ => {
                return Some((
                    2,
                    Some(Event::Key(Key::with(KeyCode::Char('['), Mods::ALT))),
                ));
            }
        }
    }
    let end = end?;
    let used = 2 + end + 1;
    let final_byte = body[end];
    let inner = &body[..end];
    let split = inner
        .iter()
        .position(|byte| (0x20..=0x2f).contains(byte))
        .unwrap_or(inner.len());
    let (params, intermediates) = inner.split_at(split);
    Some((used, decode_csi(params, intermediates, final_byte)))
}

/// The parameters of a sequence: groups separated by `;`, each of numbers
/// separated by `:`. A missing number is `None`.
fn params(text: &[u8]) -> Vec<Vec<Option<u32>>> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split(|&byte| byte == b';')
        .map(|group| {
            group
                .split(|&byte| byte == b':')
                .map(|number| {
                    std::str::from_utf8(number)
                        .ok()
                        .and_then(|number| number.parse().ok())
                })
                .collect()
        })
        .collect()
}

fn decode_csi(raw: &[u8], intermediates: &[u8], final_byte: u8) -> Option<Event> {
    let (prefix, raw) = match raw.first() {
        Some(&byte @ (b'?' | b'<' | b'=' | b'>')) => (Some(byte), &raw[1..]),
        _ => (None, raw),
    };
    let groups = params(raw);
    let number = |group: usize, index: usize| -> Option<u32> {
        groups
            .get(group)
            .and_then(|g| g.get(index))
            .copied()
            .flatten()
    };
    match (prefix, intermediates, final_byte) {
        (Some(b'?'), b"$", b'y') => Some(Event::Reply(Reply::Mode {
            mode: number(0, 0)?,
            state: number(1, 0).unwrap_or(0),
        })),
        (Some(b'?'), b"", b'u') => Some(Event::Reply(Reply::Keyboard(number(0, 0).unwrap_or(0)))),
        (Some(b'?'), b"", b'c') => Some(Event::Reply(Reply::DeviceAttributes)),
        (None, b"", b'I') if groups.is_empty() => Some(Event::Focus(true)),
        (None, b"", b'O') if groups.is_empty() => Some(Event::Focus(false)),
        (None, b"", b'u') => kitty(&groups),
        (None, b"", b'~') => tilde(number(0, 0)?, &groups),
        (None, b"", final_byte) => {
            let code = match final_byte {
                b'A' => KeyCode::Up,
                b'B' => KeyCode::Down,
                b'C' => KeyCode::Right,
                b'D' => KeyCode::Left,
                b'H' => KeyCode::Home,
                b'F' => KeyCode::End,
                b'E' => return None,
                b'P' => KeyCode::F(1),
                b'Q' => KeyCode::F(2),
                b'R' => KeyCode::F(3),
                b'S' => KeyCode::F(4),
                b'Z' => return Some(Event::Key(Key::with(KeyCode::Tab, Mods::SHIFT))),
                _ => return None,
            };
            let mods = Mods::from_param(number(1, 0).unwrap_or(1));
            Some(Event::Key(Key::with(code, mods)))
        }
        _ => None,
    }
}

/// `CSI number [; modifiers] ~`: the editing keys, function keys, the start
/// of a paste, and xterm's other keys with modifiers (`CSI 27 ; m ; c ~`).
fn tilde(first: u32, groups: &[Vec<Option<u32>>]) -> Option<Event> {
    let mods = Mods::from_param(
        groups
            .get(1)
            .and_then(|group| group.first())
            .copied()
            .flatten()
            .unwrap_or(1),
    );
    let code = match first {
        200 => return Some(Event::Paste(String::new())),
        // The end of a paste that never began: nothing.
        201 => return None,
        27 => {
            let c = groups.get(2)?.first().copied().flatten()?;
            return Some(Event::Key(Key::with(codepoint_key(c)?, mods)));
        }
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F(u8::try_from(first - 10).ok()?),
        17..=21 => KeyCode::F(u8::try_from(first - 11).ok()?),
        23..=26 => KeyCode::F(u8::try_from(first - 12).ok()?),
        28 | 29 => KeyCode::F(u8::try_from(first - 13).ok()?),
        31..=34 => KeyCode::F(u8::try_from(first - 14).ok()?),
        _ => return None,
    };
    Some(Event::Key(Key::with(code, mods)))
}

/// The key a Unicode code point stands for in a reported key.
fn codepoint_key(code: u32) -> Option<KeyCode> {
    Some(match code {
        13 => KeyCode::Enter,
        9 => KeyCode::Tab,
        127 | 8 => KeyCode::Backspace,
        27 => KeyCode::Esc,
        // The enhanced protocol's own numbers for keys that have no
        // character (the private use area) are not decoded.
        0xe000..=0xf8ff => return None,
        _ => KeyCode::Char(char::from_u32(code).filter(|c| !c.is_control())?),
    })
}

/// The enhanced keyboard protocol: `CSI code[:shifted[:base]] ; mods[:kind]
/// [; text] u`. A release (kind 3) is dropped.
fn kitty(groups: &[Vec<Option<u32>>]) -> Option<Event> {
    let code = groups.first()?.first().copied().flatten()?;
    let modifiers = groups.get(1);
    let mods = Mods::from_param(
        modifiers
            .and_then(|group| group.first())
            .copied()
            .flatten()
            .unwrap_or(1),
    );
    let kind = modifiers
        .and_then(|group| group.get(1))
        .copied()
        .flatten()
        .unwrap_or(1);
    if kind == 3 {
        return None;
    }
    Some(Event::Key(Key::with(codepoint_key(code)?, mods)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<Event> {
        let mut decoder = Decoder::new();
        let mut events = Vec::new();
        decoder.feed(bytes, &mut events);
        decoder.flush(&mut events);
        events
    }

    fn keys(list: &[(KeyCode, Mods)]) -> Vec<Event> {
        list.iter()
            .map(|&(code, mods)| Event::Key(Key::with(code, mods)))
            .collect()
    }

    const N: Mods = Mods::NONE;

    #[test]
    fn text_and_control_bytes() {
        assert_eq!(
            decode("aé漢😀".as_bytes()),
            keys(&[
                (KeyCode::Char('a'), N),
                (KeyCode::Char('é'), N),
                (KeyCode::Char('漢'), N),
                (KeyCode::Char('😀'), N),
            ])
        );
        assert_eq!(
            decode(b"\r\n\t\x7f\x08\x00\x03\x1c"),
            keys(&[
                (KeyCode::Enter, N),
                (KeyCode::Char('j'), Mods::CTRL),
                (KeyCode::Tab, N),
                (KeyCode::Backspace, N),
                (KeyCode::Backspace, Mods::CTRL),
                (KeyCode::Char(' '), Mods::CTRL),
                (KeyCode::Char('c'), Mods::CTRL),
                (KeyCode::Char('\\'), Mods::CTRL),
            ])
        );
    }

    #[test]
    fn sequences_for_the_editing_and_function_keys() {
        let cases: &[(&[u8], KeyCode, Mods)] = &[
            (b"\x1b[A", KeyCode::Up, N),
            (b"\x1bOB", KeyCode::Down, N),
            (b"\x1b[1;5C", KeyCode::Right, Mods::CTRL),
            (b"\x1b[1;3D", KeyCode::Left, Mods::ALT),
            (b"\x1b[H", KeyCode::Home, N),
            (b"\x1b[4~", KeyCode::End, N),
            (b"\x1b[3;2~", KeyCode::Delete, Mods::SHIFT),
            (b"\x1b[5~", KeyCode::PageUp, N),
            (b"\x1b[6~", KeyCode::PageDown, N),
            (b"\x1b[2~", KeyCode::Insert, N),
            (b"\x1bOP", KeyCode::F(1), N),
            (b"\x1b[1;2S", KeyCode::F(4), Mods::SHIFT),
            (b"\x1b[15~", KeyCode::F(5), N),
            (b"\x1b[24~", KeyCode::F(12), N),
            (b"\x1b[Z", KeyCode::Tab, Mods::SHIFT),
            (b"\x1b[13;2u", KeyCode::Enter, Mods::SHIFT),
            (b"\x1b[97;5u", KeyCode::Char('a'), Mods::CTRL),
            (b"\x1b[97;5:2u", KeyCode::Char('a'), Mods::CTRL),
            (b"\x1b[27;2;13~", KeyCode::Enter, Mods::SHIFT),
            (b"\x1b[1;16A", KeyCode::Up, Mods(15)),
        ];
        for (bytes, code, mods) in cases {
            assert_eq!(decode(bytes), keys(&[(*code, *mods)]), "{bytes:?}");
        }
    }

    #[test]
    fn alt_is_an_escape_before_the_key() {
        assert_eq!(
            decode(b"\x1bx\x1b\r\x1b\x7f\x1b\x1b"),
            keys(&[
                (KeyCode::Char('x'), Mods::ALT),
                (KeyCode::Enter, Mods::ALT),
                (KeyCode::Backspace, Mods::ALT),
                (KeyCode::Esc, Mods::ALT),
            ])
        );
        assert_eq!(
            decode("\x1bé".as_bytes()),
            keys(&[(KeyCode::Char('é'), Mods::ALT)])
        );
    }

    #[test]
    fn a_lone_escape_waits_and_is_the_escape_key_when_nothing_follows() {
        let mut decoder = Decoder::new();
        let mut events = Vec::new();
        decoder.feed(b"\x1b", &mut events);
        assert!(events.is_empty());
        assert!(decoder.pending());
        decoder.flush(&mut events);
        assert_eq!(events, keys(&[(KeyCode::Esc, N)]));
        assert!(!decoder.pending());
        // A sequence that never ends is Alt with its introducer, and what
        // followed is decoded as it stands.
        assert_eq!(
            decode(b"\x1b[12"),
            keys(&[
                (KeyCode::Char('['), Mods::ALT),
                (KeyCode::Char('1'), N),
                (KeyCode::Char('2'), N)
            ])
        );
    }

    #[test]
    fn releases_and_unknown_sequences_are_dropped_whole() {
        assert_eq!(decode(b"\x1b[97;1:3u"), vec![]);
        assert_eq!(decode(b"\x1b[57441;2u"), vec![]);
        assert_eq!(decode(b"\x1b[?9999z"), vec![]);
        assert_eq!(decode(b"\x1b[5x"), vec![]);
        assert_eq!(decode(b"\x1bP1$r0m\x1b\\"), vec![]);
        assert_eq!(
            decode(b"a\x1b[201~b"),
            keys(&[(KeyCode::Char('a'), N), (KeyCode::Char('b'), N)])
        );
        // A run of parameter bytes longer than any terminal sends is not
        // kept waiting for an end that will not come: the first part of it
        // is dropped.
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 400));
        let events = decode(&long);
        assert!(events.len() < 200, "{}", events.len());
    }

    #[test]
    fn a_paste_is_one_event_whatever_it_holds() {
        assert_eq!(
            decode(b"\x1b[200~line one\r\nline\x1b[Atwo\r\x1b[201~x"),
            vec![
                Event::Paste("line one\nline\u{1b}[Atwo\n".to_owned()),
                Event::Key(Key::plain(KeyCode::Char('x')))
            ]
        );
        // Waiting for the end of a paste is not something to flush.
        let mut decoder = Decoder::new();
        let mut events = Vec::new();
        decoder.feed(b"\x1b[200~abc\x1b[20", &mut events);
        decoder.flush(&mut events);
        assert!(events.is_empty());
        assert!(!decoder.pending());
        decoder.feed(b"1~", &mut events);
        assert_eq!(events, vec![Event::Paste("abc".to_owned())]);
    }

    /// `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`, as Windows Terminal sends it.
    fn record(vk: u32, unit: u32, down: bool, state: u32) -> String {
        format!("\x1b[{vk};0;{unit};{};{state};1_", u8::from(down))
    }

    #[test]
    fn win32_input_records_carry_every_modifier() {
        let cases: &[(String, KeyCode, Mods)] = &[
            (record(0x0d, 13, true, SHIFT), KeyCode::Enter, Mods::SHIFT),
            (
                record(0x0d, 13, true, LEFT_CTRL),
                KeyCode::Enter,
                Mods::CTRL,
            ),
            (record(0x09, 9, true, LEFT_CTRL), KeyCode::Tab, Mods::CTRL),
            (record(0x09, 9, true, SHIFT), KeyCode::Tab, Mods::SHIFT),
            (
                record(0x41, 1, true, LEFT_CTRL | SHIFT),
                KeyCode::Char('a'),
                Mods::CTRL | Mods::SHIFT,
            ),
            (
                record(0x41, 1, true, RIGHT_CTRL),
                KeyCode::Char('a'),
                Mods::CTRL,
            ),
            (
                record(0x58, u32::from('x'), true, LEFT_ALT),
                KeyCode::Char('x'),
                Mods::ALT,
            ),
            (
                record(0x41, u32::from('A'), true, SHIFT),
                KeyCode::Char('A'),
                Mods::NONE,
            ),
            // AltGr made the character: no modifier is left.
            (
                record(0x51, u32::from('@'), true, RIGHT_ALT | LEFT_CTRL),
                KeyCode::Char('@'),
                Mods::NONE,
            ),
            (
                record(0x20, 0, true, LEFT_CTRL),
                KeyCode::Char(' '),
                Mods::CTRL,
            ),
            (
                record(0x32, 0, true, LEFT_CTRL | SHIFT),
                KeyCode::Char('2'),
                Mods::CTRL | Mods::SHIFT,
            ),
            (
                record(0x26, 0, true, LEFT_ALT | 0x100),
                KeyCode::Up,
                Mods::ALT,
            ),
            (record(0x71, 0, true, 0), KeyCode::F(2), Mods::NONE),
            (record(0x1b, 27, true, 0), KeyCode::Esc, Mods::NONE),
            (
                record(0x08, 8, true, LEFT_CTRL),
                KeyCode::Backspace,
                Mods::CTRL,
            ),
            // Caps Lock and Num Lock are not modifiers.
            (
                record(0x41, u32::from('A'), true, 0x80 | 0x20),
                KeyCode::Char('A'),
                Mods::NONE,
            ),
        ];
        for (bytes, code, mods) in cases {
            assert_eq!(
                decode(bytes.as_bytes()),
                keys(&[(*code, *mods)]),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn win32_input_releases_and_lone_modifiers_are_dropped() {
        let mut bytes = String::new();
        bytes.push_str(&record(0x10, 0, true, SHIFT));
        bytes.push_str(&record(0x41, u32::from('A'), true, SHIFT));
        bytes.push_str(&record(0x41, u32::from('A'), false, SHIFT));
        bytes.push_str(&record(0x10, 0, false, 0));
        bytes.push_str(&record(0x11, 0, true, LEFT_CTRL));
        assert_eq!(decode(bytes.as_bytes()), keys(&[(KeyCode::Char('A'), N)]));
    }

    #[test]
    fn win32_input_joins_the_halves_of_a_character() {
        let mut bytes = String::new();
        for unit in "😀".encode_utf16() {
            bytes.push_str(&record(0, u32::from(unit), true, 0));
        }
        bytes.push('b');
        assert_eq!(
            decode(bytes.as_bytes()),
            keys(&[(KeyCode::Char('😀'), N), (KeyCode::Char('b'), N)])
        );
        // A second half alone is nothing.
        assert_eq!(decode(record(0, 0xdc00, true, 0).as_bytes()), vec![]);
    }

    #[test]
    fn a_long_paste_comes_in_pieces_and_loses_nothing() {
        let line = "a line of pasted text, é\r\n";
        let count = PASTE_PIECE / line.len() * 2 + 7;
        let mut decoder = Decoder::new();
        let mut events = Vec::new();
        decoder.feed(b"\x1b[200~", &mut events);
        let text = line.repeat(count);
        for piece in text.as_bytes().chunks(65_537) {
            decoder.feed(piece, &mut events);
        }
        assert!(events.len() >= 2, "{}", events.len());
        decoder.feed(b"\x1b[201~x", &mut events);
        let mut pasted = String::new();
        for event in &events[..events.len() - 1] {
            match event {
                Event::Paste(piece) => pasted.push_str(piece),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(pasted, text.replace("\r\n", "\n"));
        assert_eq!(
            events.last(),
            Some(&Event::Key(Key::plain(KeyCode::Char('x'))))
        );
    }

    #[test]
    fn a_piece_of_a_paste_never_ends_inside_a_character() {
        for tail in ["😀", "é", "漢", "a\r"] {
            // An invalid byte early on does not move the cut.
            let mut paste = b"x\xffy ".to_vec();
            paste.extend_from_slice(tail.as_bytes());
            let whole = paste.clone();
            for keep in 1..tail.len() {
                let mut piece = whole[..whole.len() - keep].to_vec();
                let text = paste_piece(&mut piece);
                assert!(
                    !text.ends_with('\u{fffd}'),
                    "{tail:?} less {keep}: {text:?}"
                );
                assert_eq!(
                    [text.replace('\u{fffd}', "\u{ff}").as_bytes(), &piece]
                        .concat()
                        .len()
                        - usize::from(text.contains('\u{fffd}')),
                    whole.len() - keep,
                    "{tail:?} less {keep}"
                );
            }
            let text = paste_piece(&mut paste);
            if tail == "a\r" {
                assert_eq!(paste, b"\r");
            } else {
                assert!(text.ends_with(tail), "{text:?}");
                assert!(paste.is_empty());
            }
        }
    }

    #[test]
    fn an_escape_bracket_before_a_byte_no_sequence_holds_is_alt_bracket() {
        assert_eq!(
            decode("\x1b[\ré".as_bytes()),
            keys(&[
                (KeyCode::Char('['), Mods::ALT),
                (KeyCode::Enter, N),
                (KeyCode::Char('é'), N)
            ])
        );
    }

    #[test]
    fn focus_and_replies() {
        assert_eq!(
            decode(b"\x1b[I\x1b[O"),
            vec![Event::Focus(true), Event::Focus(false)]
        );
        assert_eq!(
            decode(b"\x1b[?2026;2$y\x1b[?1u\x1b]11;rgb:1e1e/2020/ffff\x1b\\\x1b]11;rgb:ff/0/8\x07\x1b[?62;22c"),
            vec![
                Event::Reply(Reply::Mode { mode: 2026, state: 2 }),
                Event::Reply(Reply::Keyboard(1)),
                Event::Reply(Reply::Background(0x1e, 0x20, 0xff)),
                Event::Reply(Reply::Background(255, 0, 0x88)),
                Event::Reply(Reply::DeviceAttributes),
            ]
        );
        assert_eq!(decode(b"\x1b]11;rgb:zz/0/0\x07"), vec![]);
    }

    #[test]
    fn invalid_utf8_is_a_replacement_character_and_decoding_goes_on() {
        assert_eq!(
            decode(b"\xffa\xc3(b"),
            keys(&[
                (KeyCode::Char('\u{fffd}'), N),
                (KeyCode::Char('a'), N),
                (KeyCode::Char('\u{fffd}'), N),
                (KeyCode::Char('('), N),
                (KeyCode::Char('b'), N),
            ])
        );
    }

    /// The same bytes decode to the same events however a read cuts them,
    /// into two pieces or three: the property behind "a split UTF-8
    /// character or escape sequence".
    #[test]
    fn every_split_decodes_the_same() {
        let input: &[u8] = "a\x1b[1;5Cé\x1b[200~p\r\nq\x1b[201~漢\x1bOP\x1b]11;rgb:01/02/03\x1b\\😀\x1b[13;2u\x1bx\x1b[?2026;1$y\x1b[13;28;13;1;16;1_"
            .as_bytes();
        let whole = decode(input);
        assert_eq!(whole.len(), 12, "{whole:?}");
        for first in 0..=input.len() {
            for second in first..=input.len() {
                // Every third second cut, which reaches every kind of
                // boundary; the whole set in the nightly run.
                if second % 3 != 0 && !exhaustive() {
                    continue;
                }
                let mut decoder = Decoder::new();
                let mut events = Vec::new();
                decoder.feed(&input[..first], &mut events);
                decoder.feed(&input[first..second], &mut events);
                decoder.feed(&input[second..], &mut events);
                decoder.flush(&mut events);
                assert_eq!(events, whole, "cut at {first} and {second}");
            }
        }
    }

    /// Whether the nightly run asked for every case (`CORTEX_PROPERTY_SCALE`
    /// above 1). The test kit has this as `cases::exhaustive`, but unit tests
    /// of this crate cannot depend on it.
    fn exhaustive() -> bool {
        std::env::var("CORTEX_PROPERTY_SCALE")
            .ok()
            .and_then(|scale| scale.parse::<u32>().ok())
            .is_some_and(|scale| scale > 1)
    }
}
