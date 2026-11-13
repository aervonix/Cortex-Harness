//! A virtual terminal: bytes in, a screen and a scrollback out, and a
//! record of what each frame did to them.
//!
//! It is what the presenter tests write into, and what the pseudo-terminal
//! driver ([`crate::pty`]) parses a program's output with. It models an
//! xterm-like terminal closely enough for both: printing with wide
//! characters and automatic wrap, cursor movement, erasing, inserting and
//! deleting, scrolling (with margins), the alternate screen, styles, the
//! private modes the terminal layer uses, and the queries it sends. It does
//! not re-wrap on resize, as xterm does not.
//!
//! # Watching a frame
//!
//! [`VTerm::watch`] starts a record and [`VTerm::report`] ends it. The
//! [`Report`] says which rows were written to, counted from the top of the
//! scrollback so that a row keeps its number when the screen scrolls; which
//! rows were blank at some moment inside the frame although they had text
//! before and after it (a clear followed by a redraw, which a terminal
//! without synchronized updates could show as a flash); whether the frame
//! began and ended a synchronized update; and whether anything cleared the
//! whole screen.
//!
//! The checks are made after every action the bytes ask for, not only at the
//! end, so a row that is blanked and written again inside one write is seen.
//!
//! # C1 control characters
//!
//! In UTF-8, `U+009B` is taken as the start of a control sequence, exactly
//! like `ESC [`, as some terminals take it. A renderer that let one through
//! from content would be seen moving the cursor here.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use cortex_term::{Color, Style};

/// One cell of the screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// What is shown: a character with any combining marks after it. Empty
    /// for a blank cell, and for the right half of a wide character.
    pub text: String,
    /// How it is shown.
    pub style: Style,
    /// The right half of a wide character.
    pub tail: bool,
}

/// One row of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    cells: Vec<Cell>,
}

impl Row {
    fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
        }
    }

    /// The row's text, without the blank cells at its end.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = String::new();
        for cell in &self.cells {
            if cell.tail {
                continue;
            }
            if cell.text.is_empty() {
                text.push(' ');
            } else {
                text.push_str(&cell.text);
            }
        }
        text.truncate(text.trim_end_matches(' ').len());
        text
    }

    /// The cells.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    fn is_blank(&self) -> bool {
        self.cells
            .iter()
            .all(|cell| cell.text.is_empty() || cell.text == " ")
    }
}

/// A question the program asked the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// The primary device attributes (`CSI c`).
    DeviceAttributes,
    /// The state of a private mode (`CSI ? n $ p`).
    Mode(u32),
    /// The enhanced keyboard protocol's flags (`CSI ? u`).
    Keyboard,
    /// The background colour (`OSC 11 ; ?`).
    Background,
    /// The cursor's position (`CSI 6 n`).
    CursorPosition,
}

/// What one watched stretch of output did. See the [module
/// documentation](self).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The rows written to, erased or moved, counted from the top of the
    /// scrollback.
    pub touched: BTreeSet<usize>,
    /// Rows that had text before, were blank at some moment, and have text
    /// after: cleared and drawn again.
    pub cleared_then_drawn: BTreeSet<usize>,
    /// Erases of the whole screen or of the scrollback.
    pub screen_clears: usize,
    /// Whether the first thing done was to begin a synchronized update.
    pub begins_with_sync: bool,
    /// Whether the last thing done was to end one.
    pub ends_with_sync: bool,
    /// How many synchronized updates were begun and ended.
    pub syncs: (usize, usize),
    /// How many actions the bytes asked for.
    pub actions: usize,
}

impl Report {
    /// The properties every frame of the inline presenter has
    /// (`docs/testing-and-ci.md`, "Testing the TUI"), as a list of what is
    /// wrong: wrapped in one synchronized update when `sync` is on, nothing
    /// cleared and drawn again, no screen cleared, and no row above
    /// `live_top` touched.
    #[must_use]
    pub fn problems(&self, live_top: usize, sync: bool) -> Vec<String> {
        let mut problems = Vec::new();
        if sync && !(self.begins_with_sync && self.ends_with_sync && self.syncs == (1, 1)) {
            problems.push(format!(
                "not one synchronized update: begins with one {}, ends with one {}, {} begun and {} ended",
                self.begins_with_sync, self.ends_with_sync, self.syncs.0, self.syncs.1
            ));
        }
        if !self.cleared_then_drawn.is_empty() {
            problems.push(format!(
                "rows cleared and drawn again: {:?}",
                self.cleared_then_drawn
            ));
        }
        if self.screen_clears > 0 {
            problems.push(format!(
                "{} clear(s) of the whole screen",
                self.screen_clears
            ));
        }
        let above: Vec<usize> = self.touched.range(..live_top).copied().collect();
        if !above.is_empty() {
            problems.push(format!(
                "rows above the live region (which starts at {live_top}) touched: {above:?}"
            ));
        }
        problems
    }
}

#[derive(Debug, Default)]
struct Watch {
    had_text: BTreeSet<usize>,
    blanked: BTreeSet<usize>,
    report: Report,
    first: Option<bool>,
    last: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    /// After `ESC (` and its kind: one byte to skip.
    Charset,
    Csi(Vec<u8>),
    /// A string (OSC when the flag is set; DCS and the rest otherwise),
    /// and whether the last byte was ESC.
    Text(bool, Vec<u8>, bool),
}

/// The modes a test asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent modes of a terminal, each set or reset"
)]
pub struct Modes {
    /// Inside a synchronized update.
    pub sync: bool,
    /// The cursor is shown.
    pub cursor_visible: bool,
    /// Bracketed paste is on.
    pub bracketed_paste: bool,
    /// Focus events are on.
    pub focus: bool,
    /// The alternate screen is shown.
    pub alternate: bool,
    /// Automatic wrap at the right margin.
    pub autowrap: bool,
    /// Windows Terminal's win32 input mode (9001).
    pub win32_input: bool,
}

/// A virtual terminal. See the [module documentation](self).
#[derive(Debug)]
pub struct VTerm {
    cols: usize,
    rows: usize,
    screen: Vec<Row>,
    scrollback: Vec<Row>,
    /// The normal screen and the cursor, while the alternate one is shown.
    saved_screen: Option<(Vec<Row>, (usize, usize))>,
    row: usize,
    col: usize,
    pending_wrap: bool,
    style: Style,
    saved_cursor: (usize, usize, Style),
    top: usize,
    bottom: usize,
    modes: Modes,
    keyboard: Vec<u32>,
    state: State,
    utf8: Vec<u8>,
    queries: Vec<Query>,
    watch: Option<Watch>,
    title: String,
}

impl VTerm {
    /// A blank terminal of `cols` by `rows`, the cursor at the top left.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (usize::from(cols.max(1)), usize::from(rows.max(1)));
        Self {
            cols,
            rows,
            screen: vec![Row::new(cols); rows],
            scrollback: Vec::new(),
            saved_screen: None,
            row: 0,
            col: 0,
            pending_wrap: false,
            style: Style::PLAIN,
            saved_cursor: (0, 0, Style::PLAIN),
            top: 0,
            bottom: rows - 1,
            modes: Modes {
                sync: false,
                cursor_visible: true,
                bracketed_paste: false,
                focus: false,
                alternate: false,
                autowrap: true,
                win32_input: false,
            },
            keyboard: Vec::new(),
            state: State::Ground,
            utf8: Vec::new(),
            queries: Vec::new(),
            watch: None,
            title: String::new(),
        }
    }

    /// The size: columns, then rows.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (
            u16::try_from(self.cols).unwrap_or(u16::MAX),
            u16::try_from(self.rows).unwrap_or(u16::MAX),
        )
    }

    /// The cursor's row on the screen and its column.
    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// The cursor's row counted from the top of the scrollback.
    #[must_use]
    pub fn cursor_line(&self) -> usize {
        self.scrollback.len() + self.row
    }

    /// The modes.
    #[must_use]
    pub fn modes(&self) -> Modes {
        self.modes
    }

    /// The enhanced keyboard protocol's stack of flags: empty when it is
    /// off.
    #[must_use]
    pub fn keyboard(&self) -> &[u32] {
        &self.keyboard
    }

    /// The window title last set.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The text of each row of the screen, without trailing blanks.
    #[must_use]
    pub fn screen(&self) -> Vec<String> {
        self.screen.iter().map(Row::text).collect()
    }

    /// One row of the screen.
    #[must_use]
    pub fn row(&self, row: usize) -> &Row {
        &self.screen[row]
    }

    /// The text of the scrollback and the screen, top to bottom, without
    /// the blank rows at the end of the screen.
    #[must_use]
    pub fn history(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .scrollback
            .iter()
            .chain(&self.screen)
            .map(Row::text)
            .collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    /// Whether any row of the screen contains `text`.
    #[must_use]
    pub fn shows(&self, text: &str) -> bool {
        self.screen.iter().any(|row| row.text().contains(text))
    }

    /// The screen as text, one row a line, for a failure message.
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (index, row) in self.screen.iter().enumerate() {
            let marker = if index == self.row { '>' } else { ' ' };
            let _ = writeln!(out, "{marker}{index:>3}|{}", row.text());
        }
        out
    }

    /// The questions asked since the last call.
    pub fn take_queries(&mut self) -> Vec<Query> {
        std::mem::take(&mut self.queries)
    }

    /// Starts recording what the output does.
    pub fn watch(&mut self) {
        let had_text = self
            .screen
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.is_blank())
            .map(|(index, _)| self.scrollback.len() + index)
            .collect();
        self.watch = Some(Watch {
            had_text,
            ..Watch::default()
        });
    }

    /// Ends the record and says what happened.
    ///
    /// # Panics
    ///
    /// When [`watch`](Self::watch) was not called first.
    pub fn report(&mut self) -> Report {
        let watch = self.watch.take().expect("watch() before report()");
        let mut report = watch.report;
        let base = self.scrollback.len();
        report.cleared_then_drawn = watch
            .blanked
            .into_iter()
            .filter(|line| {
                line.checked_sub(base)
                    .and_then(|row| self.screen.get(row))
                    .is_some_and(|row| !row.is_blank())
            })
            .collect();
        report.begins_with_sync = watch.first == Some(true);
        report.ends_with_sync = watch.last == Some(true);
        report
    }

    /// Changes the size as a window does: nothing is re-wrapped. A shorter
    /// screen loses rows from the top into the scrollback, as many as keep
    /// the cursor on it, and then from the bottom.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (usize::from(cols.max(1)), usize::from(rows.max(1)));
        for row in self.screen.iter_mut().chain(&mut self.scrollback) {
            row.cells.resize(cols, Cell::default());
            if let Some(last) = row.cells.last_mut()
                && last.text.is_empty()
            {
                last.tail = false;
            }
        }
        while self.screen.len() > rows {
            if self.row > 0 && self.row >= rows {
                let top = self.screen.remove(0);
                if !self.modes.alternate {
                    self.scrollback.push(top);
                }
                self.row -= 1;
            } else {
                self.screen.pop();
            }
        }
        while self.screen.len() < rows {
            self.screen.push(Row::new(cols));
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.pending_wrap = false;
    }

    /// Takes in output.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    fn action(&mut self, sync: Option<bool>) {
        if let Some(watch) = &mut self.watch {
            watch.report.actions += 1;
            let is_sync = sync.is_some();
            if watch.first.is_none() {
                watch.first = Some(sync == Some(true));
            }
            watch.last = Some(is_sync && sync == Some(false));
            match sync {
                Some(true) => watch.report.syncs.0 += 1,
                Some(false) => watch.report.syncs.1 += 1,
                None => {}
            }
        }
    }

    /// Records that screen row `row` was changed: by erasing (or moving
    /// rows), or by printing. Only an erase can be the clear of a clear and
    /// redraw: text written over text, even spaces over a letter, is not.
    fn touch(&mut self, row: usize, erased: bool) {
        let line = self.scrollback.len() + row;
        let blank = erased && self.screen[row].is_blank();
        if let Some(watch) = &mut self.watch {
            watch.report.touched.insert(line);
            if blank && watch.had_text.contains(&line) {
                watch.blanked.insert(line);
            }
        }
    }

    fn byte(&mut self, byte: u8) {
        let state = std::mem::replace(&mut self.state, State::Ground);
        self.state = match state {
            State::Ground => {
                self.ground(byte);
                std::mem::replace(&mut self.state, State::Ground)
            }
            State::Escape => self.escape(byte),
            State::Charset => State::Ground,
            State::Csi(mut body) => {
                if (0x40..=0x7e).contains(&byte) {
                    self.csi(&body, byte);
                    State::Ground
                } else if byte == 0x1b {
                    State::Escape
                } else if body.len() > 512 {
                    State::Ground
                } else {
                    body.push(byte);
                    State::Csi(body)
                }
            }
            State::Text(osc, mut body, escaped) => match byte {
                0x07 if osc => {
                    self.string(osc, &body);
                    State::Ground
                }
                b'\\' if escaped => {
                    self.string(osc, &body);
                    State::Ground
                }
                0x1b => State::Text(osc, body, true),
                _ => {
                    if escaped {
                        body.push(0x1b);
                    }
                    if body.len() < 4096 {
                        body.push(byte);
                    }
                    State::Text(osc, body, false)
                }
            },
        };
    }

    fn ground(&mut self, byte: u8) {
        let need = |lead: u8| match lead {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        if !self.utf8.is_empty() {
            if (0x80..0xc0).contains(&byte) {
                self.utf8.push(byte);
                if self.utf8.len() == need(self.utf8[0]) {
                    self.flush_utf8();
                }
                return;
            }
            self.flush_utf8();
        }
        match byte {
            0x1b => self.state = State::Escape,
            0x20..=0x7e => self.char(char::from(byte)),
            0xc0.. if need(byte) > 1 => self.utf8.push(byte),
            0x80.. => self.char('\u{fffd}'),
            _ => self.control(byte),
        }
    }

    fn flush_utf8(&mut self) {
        let bytes = std::mem::take(&mut self.utf8);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        for c in text.chars() {
            self.char(c);
        }
    }

    fn char(&mut self, c: char) {
        match c {
            '\u{9b}' => self.state = State::Csi(Vec::new()),
            '\u{9d}' => self.state = State::Text(true, Vec::new(), false),
            '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                self.state = State::Text(false, Vec::new(), false);
            }
            '\u{80}'..='\u{9f}' => self.action(None),
            _ => self.print(c),
        }
    }

    fn control(&mut self, byte: u8) {
        self.action(None);
        match byte {
            b'\r' => {
                self.col = 0;
                self.pending_wrap = false;
            }
            b'\n' | 0x0b | 0x0c => self.line_feed(),
            0x08 => {
                self.col = self.col.saturating_sub(1);
                self.pending_wrap = false;
            }
            b'\t' => {
                self.col = ((self.col / 8 + 1) * 8).min(self.cols - 1);
                self.pending_wrap = false;
            }
            _ => {}
        }
    }

    fn line_feed(&mut self) {
        self.pending_wrap = false;
        if self.row == self.bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    /// Scrolls the region between the margins up by `count` rows. With the
    /// top margin at the top of the screen, the rows that leave go into the
    /// scrollback, and nothing that stays changes its number.
    fn scroll_up(&mut self, count: usize) {
        for _ in 0..count {
            let gone = self.screen.remove(self.top);
            self.screen.insert(self.bottom, Row::new(self.cols));
            if self.top == 0 && !self.modes.alternate {
                self.scrollback.push(gone);
            } else {
                for row in self.top..=self.bottom {
                    self.touch(row, true);
                }
            }
        }
    }

    fn scroll_down(&mut self, count: usize) {
        for _ in 0..count {
            self.screen.remove(self.bottom);
            self.screen.insert(self.top, Row::new(self.cols));
            for row in self.top..=self.bottom {
                self.touch(row, true);
            }
        }
    }

    fn print(&mut self, c: char) {
        self.action(None);
        let mut buffer = [0; 4];
        let width = cortex_base::text::grapheme_width(c.encode_utf8(&mut buffer));
        if width == 0 {
            // A combining mark joins the character before the cursor.
            let col = if self.pending_wrap {
                self.col
            } else {
                self.col.saturating_sub(1)
            };
            let mut col = col;
            while col > 0 && self.screen[self.row].cells[col].tail {
                col -= 1;
            }
            self.screen[self.row].cells[col].text.push(c);
            self.touch(self.row, false);
            return;
        }
        if self.pending_wrap || (width == 2 && self.col + 1 >= self.cols) {
            if self.modes.autowrap {
                self.col = 0;
                self.line_feed();
            } else {
                self.col = self.cols - width.min(self.cols);
            }
            self.pending_wrap = false;
        }
        let (row, col) = (self.row, self.col);
        self.clear_wide(row, col);
        if width == 2 && col + 1 < self.cols {
            self.clear_wide(row, col + 1);
        }
        let cells = &mut self.screen[row].cells;
        cells[col] = Cell {
            text: c.to_string(),
            style: self.style,
            tail: false,
        };
        if width == 2 && col + 1 < self.cols {
            cells[col + 1] = Cell {
                text: String::new(),
                style: self.style,
                tail: true,
            };
        }
        self.touch(row, false);
        let next = col + width;
        if next >= self.cols {
            self.col = self.cols - 1;
            self.pending_wrap = true;
        } else {
            self.col = next;
        }
    }

    /// Before cell `col` is written: if it is half of a wide character, the
    /// other half goes blank.
    fn clear_wide(&mut self, row: usize, col: usize) {
        let cells = &mut self.screen[row].cells;
        if cells[col].tail && col > 0 {
            cells[col - 1] = Cell::default();
        } else if col + 1 < cells.len() && cells[col + 1].tail {
            cells[col + 1] = Cell::default();
        }
    }

    fn escape(&mut self, byte: u8) -> State {
        match byte {
            b'[' => return State::Csi(Vec::new()),
            b']' => return State::Text(true, Vec::new(), false),
            b'P' | b'X' | b'^' | b'_' => return State::Text(false, Vec::new(), false),
            b'(' | b')' | b'*' | b'+' => return State::Charset,
            _ => {}
        }
        self.action(None);
        match byte {
            b'7' => self.saved_cursor = (self.row, self.col, self.style),
            b'8' => {
                (self.row, self.col, self.style) = self.saved_cursor;
                self.pending_wrap = false;
            }
            b'D' => self.line_feed(),
            b'E' => {
                self.col = 0;
                self.line_feed();
            }
            b'M' => {
                self.pending_wrap = false;
                if self.row == self.top {
                    self.scroll_down(1);
                } else {
                    self.row = self.row.saturating_sub(1);
                }
            }
            b'c' => {
                let (cols, rows) = self.size();
                let scrollback = std::mem::take(&mut self.scrollback);
                let watch = self.watch.take();
                *self = Self::new(cols, rows);
                self.scrollback = scrollback;
                self.watch = watch;
                if let Some(watch) = &mut self.watch {
                    watch.report.screen_clears += 1;
                }
            }
            _ => {}
        }
        State::Ground
    }

    fn string(&mut self, osc: bool, body: &[u8]) {
        self.action(None);
        if !osc {
            return;
        }
        let text = String::from_utf8_lossy(body);
        let (kind, rest) = text.split_once(';').unwrap_or((&text, ""));
        match kind {
            "0" | "2" => self.title = rest.to_owned(),
            "11" if rest == "?" => self.queries.push(Query::Background),
            _ => {}
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one match over the control sequences the terminal knows"
    )]
    fn csi(&mut self, body: &[u8], final_byte: u8) {
        let (prefix, body) = match body.first() {
            Some(&byte @ (b'?' | b'>' | b'<' | b'=')) => (Some(byte), &body[1..]),
            _ => (None, body),
        };
        let split = body
            .iter()
            .position(|byte| (0x20..=0x2f).contains(byte))
            .unwrap_or(body.len());
        let (params, intermediates) = body.split_at(split);
        let numbers: Vec<Option<usize>> = if params.is_empty() {
            Vec::new()
        } else {
            params
                .split(|&b| b == b';' || b == b':')
                .map(|n| std::str::from_utf8(n).ok().and_then(|n| n.parse().ok()))
                .collect()
        };
        let arg = |index: usize, default: usize| -> usize {
            numbers.get(index).copied().flatten().unwrap_or(default)
        };
        let count = arg(0, 1).max(1);

        let sync = match (prefix, intermediates, final_byte) {
            (Some(b'?'), b"", b'h' | b'l') if numbers.contains(&Some(2026)) => {
                Some(final_byte == b'h')
            }
            _ => None,
        };
        self.action(sync);

        if !matches!(final_byte, b'm' | b'h' | b'l') {
            self.pending_wrap = false;
        }
        match (prefix, intermediates, final_byte) {
            (None, b"", b'A') => {
                self.row = self.row.saturating_sub(count).max(self.top.min(self.row))
            }
            (None, b"", b'B') => {
                self.row = (self.row + count)
                    .min(self.bottom.max(self.row))
                    .min(self.rows - 1)
            }
            (None, b"", b'C' | b'a') => self.col = (self.col + count).min(self.cols - 1),
            (None, b"", b'D') => self.col = self.col.saturating_sub(count),
            (None, b"", b'E') => {
                self.row = (self.row + count).min(self.rows - 1);
                self.col = 0;
            }
            (None, b"", b'F') => {
                self.row = self.row.saturating_sub(count);
                self.col = 0;
            }
            (None, b"", b'G' | b'`') => self.col = (count - 1).min(self.cols - 1),
            (None, b"", b'd') => self.row = (count - 1).min(self.rows - 1),
            (None, b"", b'H' | b'f') => {
                self.row = (arg(0, 1).max(1) - 1).min(self.rows - 1);
                self.col = (arg(1, 1).max(1) - 1).min(self.cols - 1);
            }
            (None, b"", b'J') => self.erase_display(arg(0, 0)),
            (None, b"", b'K') => self.erase_line(arg(0, 0)),
            (None, b"", b'X') => {
                let end = (self.col + count).min(self.cols);
                self.blank(self.row, self.col, end);
            }
            (None, b"", b'@') => {
                let cells = &mut self.screen[self.row].cells;
                for _ in 0..count.min(self.cols - self.col) {
                    cells.pop();
                    cells.insert(self.col, Cell::default());
                }
                self.touch(self.row, true);
            }
            (None, b"", b'P') => {
                let cells = &mut self.screen[self.row].cells;
                for _ in 0..count.min(self.cols - self.col) {
                    cells.remove(self.col);
                    cells.push(Cell::default());
                }
                self.touch(self.row, true);
            }
            (None, b"", b'L') if (self.top..=self.bottom).contains(&self.row) => {
                for _ in 0..count.min(self.bottom - self.row + 1) {
                    self.screen.remove(self.bottom);
                    self.screen.insert(self.row, Row::new(self.cols));
                }
                for row in self.row..=self.bottom {
                    self.touch(row, true);
                }
            }
            (None, b"", b'M') if (self.top..=self.bottom).contains(&self.row) => {
                for _ in 0..count.min(self.bottom - self.row + 1) {
                    self.screen.remove(self.row);
                    self.screen.insert(self.bottom, Row::new(self.cols));
                }
                for row in self.row..=self.bottom {
                    self.touch(row, true);
                }
            }
            (None, b"", b'S') => self.scroll_up(count),
            (None, b"", b'T') => self.scroll_down(count),
            (None, b"", b'r') => {
                let top = arg(0, 1).max(1) - 1;
                let bottom = arg(1, self.rows).max(1) - 1;
                if top < bottom && bottom < self.rows {
                    self.top = top;
                    self.bottom = bottom;
                } else {
                    self.top = 0;
                    self.bottom = self.rows - 1;
                }
                self.row = 0;
                self.col = 0;
            }
            (None, b"", b's') => self.saved_cursor = (self.row, self.col, self.style),
            (None, b"", b'u') => (self.row, self.col, self.style) = self.saved_cursor,
            (None, b"", b'm') => self.sgr(&numbers),
            (None, b"", b'c') if arg(0, 0) == 0 => self.queries.push(Query::DeviceAttributes),
            (None, b"", b'n') if arg(0, 0) == 6 => self.queries.push(Query::CursorPosition),
            (Some(b'?'), b"$", b'p') => self
                .queries
                .push(Query::Mode(u32::try_from(arg(0, 0)).unwrap_or(0))),
            (Some(b'?'), b"", b'u') => self.queries.push(Query::Keyboard),
            (Some(b'>'), b"", b'u') => self.keyboard.push(u32::try_from(arg(0, 0)).unwrap_or(0)),
            (Some(b'<'), b"", b'u') => {
                for _ in 0..count {
                    self.keyboard.pop();
                }
            }
            (Some(b'?'), b"", b'h' | b'l') => {
                let on = final_byte == b'h';
                for mode in numbers.iter().flatten() {
                    self.private_mode(*mode, on);
                }
            }
            _ => {}
        }
    }

    fn private_mode(&mut self, mode: usize, on: bool) {
        match mode {
            7 => self.modes.autowrap = on,
            25 => self.modes.cursor_visible = on,
            1004 => self.modes.focus = on,
            2004 => self.modes.bracketed_paste = on,
            2026 => self.modes.sync = on,
            9001 => self.modes.win32_input = on,
            47 | 1047 | 1049 => {
                if on && !self.modes.alternate {
                    let screen =
                        std::mem::replace(&mut self.screen, vec![Row::new(self.cols); self.rows]);
                    self.saved_screen = Some((screen, (self.row, self.col)));
                    self.modes.alternate = true;
                } else if !on && self.modes.alternate {
                    if let Some((screen, (row, col))) = self.saved_screen.take() {
                        self.screen = screen;
                        if mode == 1049 {
                            self.row = row;
                            self.col = col;
                        }
                    }
                    self.modes.alternate = false;
                }
                for row in 0..self.rows {
                    self.touch(row, true);
                }
            }
            _ => {}
        }
    }

    fn blank(&mut self, row: usize, from: usize, to: usize) {
        let style = Style {
            bg: self.style.bg,
            ..Style::PLAIN
        };
        if from < to {
            if from > 0 && self.screen[row].cells[from].tail {
                self.screen[row].cells[from - 1] = Cell::default();
            }
            if to < self.cols && self.screen[row].cells[to].tail {
                self.screen[row].cells[to] = Cell::default();
            }
        }
        for cell in &mut self.screen[row].cells[from..to] {
            *cell = Cell {
                text: String::new(),
                style,
                tail: false,
            };
        }
        self.touch(row, true);
    }

    fn erase_line(&mut self, how: usize) {
        let (from, to) = match how {
            0 => (self.col, self.cols),
            1 => (0, self.col + 1),
            _ => (0, self.cols),
        };
        self.blank(self.row, from, to);
    }

    fn erase_display(&mut self, how: usize) {
        match how {
            0 => {
                self.erase_line(0);
                for row in self.row + 1..self.rows {
                    self.blank(row, 0, self.cols);
                }
            }
            1 => {
                for row in 0..self.row {
                    self.blank(row, 0, self.cols);
                }
                self.erase_line(1);
            }
            2 => {
                if let Some(watch) = &mut self.watch {
                    watch.report.screen_clears += 1;
                }
                for row in 0..self.rows {
                    self.blank(row, 0, self.cols);
                }
            }
            3 => {
                if let Some(watch) = &mut self.watch {
                    watch.report.screen_clears += 1;
                }
                self.scrollback.clear();
            }
            _ => {}
        }
    }

    fn sgr(&mut self, numbers: &[Option<usize>]) {
        if numbers.is_empty() {
            self.style = Style::PLAIN;
            return;
        }
        let mut index = 0;
        let number = |index: usize| numbers.get(index).copied().flatten().unwrap_or(0);
        let byte = |value: usize| u8::try_from(value).unwrap_or(u8::MAX);
        while index < numbers.len() {
            let n = number(index);
            let style = &mut self.style;
            match n {
                0 => *style = Style::PLAIN,
                1 => style.bold = true,
                2 => style.dim = true,
                3 => style.italic = true,
                4 => style.underline = true,
                7 => style.reverse = true,
                22 => {
                    style.bold = false;
                    style.dim = false;
                }
                23 => style.italic = false,
                24 => style.underline = false,
                27 => style.reverse = false,
                30..=37 => style.fg = Color::Ansi(byte(n - 30)),
                39 => style.fg = Color::Default,
                40..=47 => style.bg = Color::Ansi(byte(n - 40)),
                49 => style.bg = Color::Default,
                90..=97 => style.fg = Color::Ansi(byte(n - 90 + 8)),
                100..=107 => style.bg = Color::Ansi(byte(n - 100 + 8)),
                38 | 48 => {
                    let color = match number(index + 1) {
                        5 => {
                            index += 2;
                            Color::Indexed(byte(number(index)))
                        }
                        2 => {
                            index += 4;
                            Color::Rgb(
                                byte(number(index - 2)),
                                byte(number(index - 1)),
                                byte(number(index)),
                            )
                        }
                        _ => Color::Default,
                    };
                    if n == 38 {
                        style.fg = color;
                    } else {
                        style.bg = color;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }
}

/// What a modern terminal answers to `query`, for a driver that plays one.
#[must_use]
pub fn answer(query: Query, term: &VTerm) -> Vec<u8> {
    match query {
        Query::DeviceAttributes => b"\x1b[?62;22c".to_vec(),
        Query::Mode(mode) => {
            // Synchronized updates and the win32 input mode are known and
            // off; anything else is not known.
            let state = if mode == 2026 || mode == 9001 { 2 } else { 0 };
            format!("\x1b[?{mode};{state}$y").into_bytes()
        }
        Query::Keyboard => b"\x1b[?0u".to_vec(),
        Query::Background => b"\x1b]11;rgb:1e1e/1e1e/2828\x1b\\".to_vec(),
        Query::CursorPosition => {
            let (row, col) = term.cursor();
            format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: u16, rows: u16, bytes: &[u8]) -> VTerm {
        let mut term = VTerm::new(cols, rows);
        term.feed(bytes);
        term
    }

    #[test]
    fn printing_wrapping_and_scrolling() {
        let t = term(5, 3, b"abcdefg\r\nxy\r\nz\r\nlast");
        assert_eq!(t.history(), ["abcde", "fg", "xy", "z", "last"]);
        assert_eq!(t.screen(), ["xy", "z", "last"]);
        assert_eq!(t.cursor(), (2, 4));
        // A full row leaves the cursor waiting at its end: a carriage return
        // clears the wait.
        let t = term(3, 2, b"abc\rX");
        assert_eq!(t.screen(), ["Xbc", ""]);
    }

    #[test]
    fn wide_characters_and_combining_marks() {
        let t = term(5, 2, "a漢e\u{301}字".as_bytes());
        assert_eq!(
            t.screen(),
            ["a漢é", "字"].map(|s| s.replace('é', "e\u{301}"))
        );
        let t = term(4, 1, "漢字\rx".as_bytes());
        assert_eq!(t.screen(), ["x 字"]);
    }

    #[test]
    fn cursor_movement_and_erasing() {
        let t = term(
            10,
            3,
            b"one\r\ntwo\r\nthree\x1b[2A\x1b[2D\x1b[K\x1b[3;2H\x1b[1X",
        );
        assert_eq!(t.screen(), ["one", "two", "t ree"]);
        let t = term(10, 3, b"one\r\ntwo\r\nthree\x1b[2;1H\x1b[J");
        assert_eq!(t.screen(), ["one", "", ""]);
        let t = term(10, 3, b"one\r\ntwo\x1b[1;1H\x1b[L");
        assert_eq!(t.screen(), ["", "one", "two"]);
    }

    #[test]
    fn styles_and_modes() {
        let mut t = term(10, 2, b"\x1b[1;38;5;99mA\x1b[0;48;2;1;2;3mB\x1b[mC");
        let cells = t.row(0).cells();
        assert_eq!(cells[0].style, Style::PLAIN.bold().fg(Color::Indexed(99)));
        assert_eq!(cells[1].style, Style::PLAIN.bg(Color::Rgb(1, 2, 3)));
        assert_eq!(cells[2].style, Style::PLAIN);
        t.feed(b"\x1b[?25l\x1b[?2004h\x1b[?1004h\x1b[>1u\x1b[?2026h");
        let modes = t.modes();
        assert!(!modes.cursor_visible && modes.bracketed_paste && modes.focus && modes.sync);
        assert_eq!(t.keyboard(), [1]);
        t.feed(b"\x1b[<u\x1b[?2026l\x1b]0;a title\x07");
        assert!(t.keyboard().is_empty() && !t.modes().sync);
        assert_eq!(t.title(), "a title");
    }

    #[test]
    fn queries_are_recorded() {
        let mut t = term(10, 2, b"\x1b[?2026$p\x1b[?u\x1b]11;?\x1b\\\x1b[c\x1b[6n");
        assert_eq!(
            t.take_queries(),
            [
                Query::Mode(2026),
                Query::Keyboard,
                Query::Background,
                Query::DeviceAttributes,
                Query::CursorPosition
            ]
        );
        assert!(t.take_queries().is_empty());
    }

    #[test]
    fn a_c1_control_sequence_is_obeyed_as_some_terminals_do() {
        let t = term(10, 2, "ab\u{9b}1Dc".as_bytes());
        assert_eq!(t.screen(), ["ac", ""]);
    }

    #[test]
    fn a_watch_sees_a_clear_followed_by_a_redraw() {
        let mut t = term(10, 4, b"keep\r\nlive");
        t.watch();
        t.feed(b"\x1b[?2026h\r\x1b[Klive\x1b[?2026l");
        let report = t.report();
        assert_eq!(report.cleared_then_drawn, BTreeSet::from([1]));
        assert!(report.begins_with_sync && report.ends_with_sync);
        assert_eq!(report.problems(1, true).len(), 1);

        t.watch();
        t.feed(b"\x1b[?2026h\rLIVE\x1b[K\x1b[?2026l");
        let report = t.report();
        assert!(report.problems(1, true).is_empty(), "{report:?}");
        assert_eq!(report.touched, BTreeSet::from([1]));
        // Touching the row above the live region is a problem.
        assert_eq!(report.problems(2, true).len(), 1);

        t.watch();
        t.feed(b"\x1b[2J");
        let report = t.report();
        assert_eq!(report.screen_clears, 1);
        assert!(!report.begins_with_sync);
        assert_eq!(report.problems(0, true).len(), 2);
        assert_eq!(report.problems(0, false).len(), 1);
    }

    #[test]
    fn scrolling_keeps_the_numbers_of_rows() {
        let mut t = term(10, 2, b"a\r\nb");
        t.watch();
        t.feed(b"\r\nc");
        let report = t.report();
        assert_eq!(report.touched, BTreeSet::from([2]));
        assert_eq!(t.history(), ["a", "b", "c"]);
        assert_eq!(t.cursor_line(), 2);
    }

    #[test]
    fn a_resize_loses_rows_from_the_top_to_keep_the_cursor() {
        let mut t = term(10, 4, b"1\r\n2\r\n3\r\n4");
        t.resize(5, 2);
        assert_eq!(t.screen(), ["3", "4"]);
        assert_eq!(t.history(), ["1", "2", "3", "4"]);
        t.resize(5, 3);
        assert_eq!(t.screen(), ["3", "4", ""]);
    }

    #[test]
    fn answers_are_those_of_a_modern_terminal() {
        let t = term(10, 2, b"ab");
        assert_eq!(answer(Query::CursorPosition, &t), b"\x1b[1;3R");
        assert_eq!(answer(Query::Mode(2026), &t), b"\x1b[?2026;2$y");
    }
}
