//! The demo: fake streamed text on the terminal layer, for measuring it and
//! for testing it in a real terminal.
//!
//! **Its look is a placeholder.** The rule, the prompt mark, the status
//! line's words and where each sits are made up for the demo, named
//! `PLACEHOLDER_…` below, and are not the interface's: the interface's look
//! is chosen in the design round before step 21. Nothing here is in
//! `cortex-term`'s public API.
//!
//! It is the example `demo` (`cargo run --release -p cortex-term --example
//! demo --features demo -- rate=2000 auto`), and the same code runs as a
//! child of `cortex-term`'s own test binary in its system tests and in
//! `cargo xtask perf`.
//!
//! # Options
//!
//! Each is `name=value`, or a bare `name` for a switch:
//!
//! | Option | Meaning |
//! |---|---|
//! | `rate=N` | Tokens a second while streaming (400). |
//! | `turn-ms=N` | How long a turn streams (12000). |
//! | `live-rows=N` | Keep the last N lines of the text in the live region instead of committing them at once: the heavy case (1). |
//! | `auto` | Start a turn at once, without waiting for Enter. |
//! | `quit-after-turn` | Quit when the turn ends. |
//! | `panic-on=C` | Panic when the key C is typed. |
//! | `terminate-on=C` | End the process as SIGTERM or Ctrl+Break would when C is typed. |
//! | `stats=PATH` | Write the frame and key timings there on exit, as JSON. |
//!
//! # Keys
//!
//! Enter starts a turn; text goes into the editor line; Backspace deletes;
//! Ctrl+L redraws; Ctrl+Z suspends (Unix); Ctrl+C or Ctrl+D quits.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "a program of its own: it writes its statistics file and streams at a set pace"
)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use cortex_term::{
    Capabilities, Color, Cursor, Event, Frame, Inline, Key, KeyCode, Line, Mods, Overrides,
    Scheduler, Span, Style, Terminal, Wake,
};

/// PLACEHOLDER: the rule between the text and the editor.
const PLACEHOLDER_RULE: (&str, &str) = ("─", "-");
/// PLACEHOLDER: the mark before the editor's text.
const PLACEHOLDER_PROMPT: &str = "> ";
/// PLACEHOLDER: the colour of the status line.
const PLACEHOLDER_STATUS_STYLE: Style = Style::PLAIN.dim();
/// PLACEHOLDER: the colour of the marker words.
const PLACEHOLDER_MARKER_STYLE: Style = Style::PLAIN.fg(Color::Ansi(5));
/// The status line's first word: what the tests and `xtask perf` look for
/// to know a frame is complete.
pub const STATUS_MARK: &str = "demo:";
/// Made-up words the stream is built from.
const WORDS: &[&str] = &[
    "the",
    "stream",
    "carries",
    "plain",
    "words",
    "of",
    "varied",
    "length",
    "so",
    "that",
    "lines",
    "wrap",
    "at",
    "different",
    "places",
    "and",
    "commit",
    "as",
    "soon",
    "stable",
];

/// What the demo was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Tokens a second.
    pub rate: u64,
    /// How long a turn streams.
    pub turn: Duration,
    /// Lines of text kept live.
    pub live_rows: usize,
    /// Start a turn at once.
    pub auto: bool,
    /// Quit when the turn ends.
    pub quit_after_turn: bool,
    /// Panic on this key.
    pub panic_on: Option<char>,
    /// End the process as a signal would on this key.
    pub terminate_on: Option<char>,
    /// Where to write the timings.
    pub stats: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rate: 400,
            turn: Duration::from_secs(12),
            live_rows: 1,
            auto: false,
            quit_after_turn: false,
            panic_on: None,
            terminate_on: None,
            stats: None,
        }
    }
}

impl Options {
    /// Reads `name=value` options.
    ///
    /// # Errors
    ///
    /// When an option is not known, or its value is not a number where one
    /// is needed.
    pub fn parse<'a>(arguments: impl IntoIterator<Item = &'a str>) -> Result<Self, String> {
        let mut options = Self::default();
        for argument in arguments {
            let (name, value) = argument.split_once('=').unwrap_or((argument, ""));
            let number = || {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("`{name}` takes a number, not `{value}`"))
            };
            let key = || value.chars().next().ok_or(format!("`{name}` takes a key"));
            match name {
                "rate" => options.rate = number()?.max(1),
                "turn-ms" => options.turn = Duration::from_millis(number()?),
                "live-rows" => {
                    options.live_rows = usize::try_from(number()?.max(1)).unwrap_or(1);
                }
                "auto" => options.auto = true,
                "quit-after-turn" => options.quit_after_turn = true,
                "panic-on" => options.panic_on = Some(key()?),
                "terminate-on" => options.terminate_on = Some(key()?),
                "stats" => options.stats = Some(value.to_owned()),
                "" => {}
                _ => return Err(format!("unknown option `{name}`")),
            }
        }
        Ok(options)
    }
}

/// The text streamed so far and not yet committed.
#[derive(Debug, Default)]
struct Text {
    /// Wrapped lines not yet drawn: committed by the next frame.
    commit: Vec<Line>,
    /// Lines kept in the live region (the heavy case), and the growing last
    /// line.
    live: Vec<Line>,
    current: Vec<Span>,
    current_width: usize,
    tokens: u64,
}

impl Text {
    fn push(&mut self, token: u64, width: usize, keep: usize) {
        self.tokens += 1;
        let marker = token % 10 == 9;
        let word = if marker {
            format!("m{token}")
        } else {
            WORDS[usize::try_from(token).unwrap_or(0) % WORDS.len()].to_owned()
        };
        let word_width = word.len();
        if self.current_width > 0 && self.current_width + 1 + word_width > width {
            self.end_line(keep);
        }
        if self.current_width > 0 {
            self.current.push(Span::plain(" "));
            self.current_width += 1;
        }
        let style = if marker {
            PLACEHOLDER_MARKER_STYLE
        } else {
            Style::PLAIN
        };
        self.current.push(Span::new(word, style));
        self.current_width += word_width;
        if token % 97 == 96 {
            // A paragraph ends.
            self.end_line(keep);
            self.live.push(Line::new());
            self.settle(keep);
        }
    }

    fn end_line(&mut self, keep: usize) {
        let line = Line::from(std::mem::take(&mut self.current));
        self.current_width = 0;
        self.live.push(line);
        self.settle(keep);
    }

    /// Commits whatever is beyond the lines kept live: as soon as stable.
    fn settle(&mut self, keep: usize) {
        let room = keep.saturating_sub(1);
        if self.live.len() > room {
            let excess = self.live.len() - room;
            self.commit.extend(self.live.drain(..excess));
        }
    }

    fn finish(&mut self) {
        if self.current_width > 0 {
            self.end_line(0);
        }
        self.commit.append(&mut self.live);
    }
}

/// The stream's side: tokens waiting to be taken, and whether it ended.
#[derive(Debug, Default)]
struct Stream {
    tokens: Mutex<Vec<u64>>,
    done: AtomicBool,
}

/// The timings the demo keeps.
#[derive(Debug, Default)]
struct Stats {
    /// Each frame: microseconds to build and write it, and its bytes.
    frames: Vec<(u128, usize)>,
    /// Each key: microseconds from reading it to the frame that shows it
    /// being written.
    keys: Vec<u128>,
}

impl Stats {
    fn json(&self) -> String {
        let frames: Vec<String> = self
            .frames
            .iter()
            .map(|(micros, bytes)| format!("[{micros},{bytes}]"))
            .collect();
        let keys: Vec<String> = self.keys.iter().map(u128::to_string).collect();
        format!(
            "{{\"frames\":[{}],\"keys\":[{}]}}\n",
            frames.join(","),
            keys.join(",")
        )
    }
}

fn start_turn(stream: &Arc<Stream>, options: &Options, waker: cortex_term::Waker) {
    stream.done.store(false, Ordering::SeqCst);
    let stream = Arc::clone(stream);
    let (rate, turn) = (options.rate, options.turn);
    std::thread::spawn(move || {
        let started = Instant::now();
        let total = u64::try_from(turn.as_millis()).unwrap_or(u64::MAX) * rate / 1000;
        let mut sent = 0;
        while sent < total {
            let due =
                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX) * rate / 1_000_000;
            let due = due.min(total);
            if due > sent {
                stream
                    .tokens
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend(sent..due);
                sent = due;
                waker.wake();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        stream.done.store(true, Ordering::SeqCst);
        waker.wake();
    });
}

fn live_lines(
    text: &Text,
    editor: &str,
    streaming: bool,
    caps: &Capabilities,
    cols: usize,
) -> (Vec<Line>, Cursor) {
    let mut lines = text.live.clone();
    lines.push(Line::from(text.current.clone()));
    let rule = if caps.unicode {
        PLACEHOLDER_RULE.0
    } else {
        PLACEHOLDER_RULE.1
    };
    lines.push(Line::styled(rule.repeat(cols), PLACEHOLDER_STATUS_STYLE));
    let editor_row = lines.len();
    lines.push(Line::plain(format!("{PLACEHOLDER_PROMPT}{editor}")));
    let state = if streaming { "streaming" } else { "ready" };
    lines.push(Line::styled(
        format!("{STATUS_MARK} {state}, {} tokens", text.tokens),
        PLACEHOLDER_STATUS_STYLE,
    ));
    let col = cortex_base::text::width(PLACEHOLDER_PROMPT) + cortex_base::text::width(editor);
    (
        lines,
        Cursor {
            row: editor_row,
            col,
        },
    )
}

/// Runs the demo on the terminal until it is told to quit.
///
/// # Errors
///
/// When the terminal cannot be used.
///
/// # Panics
///
/// When `panic-on` asks for it.
pub fn run(options: &Options) -> io::Result<()> {
    let mut terminal = Terminal::enter(Overrides::default())?;
    let (mut cols, rows) = terminal.size()?;
    let mut presenter = Inline::new(cols, rows);
    let mut scheduler = Scheduler::new();
    let stream = Arc::new(Stream::default());
    let mut text = Text::default();
    let mut editor = String::new();
    let mut streaming = false;
    let mut stats = Stats::default();
    let mut key_read: Option<Instant> = None;

    if options.auto {
        streaming = true;
        start_turn(&stream, options, terminal.waker());
    }
    scheduler.input();
    loop {
        let tokens =
            std::mem::take(&mut *stream.tokens.lock().unwrap_or_else(PoisonError::into_inner));
        if !tokens.is_empty() {
            for token in tokens {
                text.push(token, usize::from(cols), options.live_rows);
            }
            scheduler.stream();
        }
        if streaming
            && stream.done.load(Ordering::SeqCst)
            && stream.tokens.lock().map_or(true, |t| t.is_empty())
        {
            streaming = false;
            text.finish();
            scheduler.stream();
            if options.quit_after_turn {
                break;
            }
        }
        let now = Instant::now();
        let timeout = match scheduler.wake(now) {
            Wake::Now => {
                let started = Instant::now();
                let (live, cursor) = live_lines(
                    &text,
                    &editor,
                    streaming,
                    terminal.caps(),
                    usize::from(cols),
                );
                let commit = std::mem::take(&mut text.commit);
                let caps = *terminal.caps();
                let drawn = presenter.draw(
                    &Frame {
                        commit: &commit,
                        live: &live,
                        cursor: Some(cursor),
                    },
                    &caps,
                    &mut terminal,
                )?;
                let finished = Instant::now();
                if let cortex_term::Drawn::Written(bytes) = drawn {
                    stats.frames.push(((finished - started).as_micros(), bytes));
                }
                if let Some(read) = key_read.take() {
                    stats.keys.push((finished - read).as_micros());
                }
                scheduler.drawn(now);
                continue;
            }
            Wake::At(at) => Some(at.saturating_duration_since(now)),
            Wake::Idle => None,
        };
        let Some(event) = terminal.next(timeout)? else {
            continue;
        };
        match event {
            Event::Key(Key { code, mods }) if mods.contains(Mods::CTRL) => match code {
                KeyCode::Char('c' | 'd') => break,
                KeyCode::Char('l') => {
                    presenter.redraw();
                    scheduler.input();
                }
                KeyCode::Char('z') => {
                    presenter.finish(&terminal.caps().clone(), &mut terminal)?;
                    terminal.suspend();
                }
                _ => {}
            },
            Event::Key(Key { code, .. }) => {
                key_read = Some(Instant::now());
                match code {
                    KeyCode::Char(c) if Some(c) == options.panic_on => {
                        panic!("demo panic on request");
                    }
                    KeyCode::Char(c) if Some(c) == options.terminate_on => {
                        cortex_term::pty::terminate_self()?;
                    }
                    KeyCode::Char(c) => editor.push(c),
                    KeyCode::Backspace => {
                        editor.pop();
                    }
                    KeyCode::Enter if !streaming => {
                        editor.clear();
                        streaming = true;
                        start_turn(&stream, options, terminal.waker());
                    }
                    _ => {}
                }
                scheduler.input();
            }
            Event::Paste(pasted) => {
                editor.push_str(&cortex_term::sanitize(&pasted).replace(['\n', '\t'], " "));
                scheduler.input();
            }
            Event::Resize {
                cols: new_cols,
                rows,
            } => {
                cols = new_cols;
                presenter.resize(new_cols, rows);
                scheduler.input();
            }
            Event::Resumed => {
                presenter.restart();
                scheduler.input();
            }
            Event::Focus(_) | Event::Reply(_) => {}
        }
    }
    let caps = *terminal.caps();
    presenter.finish(&caps, &mut terminal)?;
    terminal.leave()?;
    if let Some(path) = &options.stats {
        std::fs::write(path, stats.json())?;
    }
    Ok(())
}
