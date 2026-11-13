//! The inline presenter: committed lines go into the terminal's own
//! scrollback, and a small live region at the bottom is redrawn in place
//! (`docs/architecture/09-tui.md`, "How a frame is drawn (inline)").
//!
//! # A frame
//!
//! A [`Frame`] is the lines committed since the last one, the lines of the
//! live region, and where the cursor goes. [`Inline::draw`] builds the whole
//! frame in one buffer and hands it to a [`Sink`] in **one** call:
//!
//! 1. begin a synchronized update, and hide the cursor;
//! 2. go to the first row of the live region (a carriage return and a move
//!    up: never above it);
//! 3. write each committed line, erase the rest of its row, and go to the next
//!    row; the terminal scrolls by itself at the bottom;
//! 4. write the live region: when nothing was committed and its height did not
//!    change, only the rows that differ; otherwise every row, then erase
//!    whatever of the old region is left below;
//! 5. put the cursor where the frame says, and show it;
//! 6. end the synchronized update.
//!
//! Nothing is cleared and then drawn again: each row is written over in
//! place, and erasing is only ever of the part of a row after its text, or of
//! rows below the region. No scroll region is set and no cursor position is
//! asked for.
//!
//! # What it does not know
//!
//! The presenter does not ask the terminal where the cursor is. It keeps
//! the row the cursor was left on, inside the live region, and moves from
//! there. After a resize a terminal may have re-wrapped the live region
//! itself; the presenter then redraws from where it believes the region
//! starts, and a few stale rows may be left in the scrollback after a
//! narrowing (`09-tui.md`, "Resize").

use std::io;

use crate::caps::Capabilities;
use crate::line::Line;
use crate::render::{self, Target};

/// Begin a synchronized update: the terminal shows nothing until the end.
pub(crate) const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
/// End a synchronized update.
pub(crate) const SYNC_END: &[u8] = b"\x1b[?2026l";
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
/// Erase from the cursor to the end of its row.
const ERASE_LINE: &[u8] = b"\x1b[K";
/// Erase from the cursor to the end of the screen.
const ERASE_BELOW: &[u8] = b"\x1b[J";

/// Where a frame's bytes go. A presenter makes exactly one call for each
/// frame it draws: that is what lets a terminal without synchronized updates
/// receive a frame in one piece.
pub trait Sink {
    /// Writes one whole frame.
    ///
    /// # Errors
    ///
    /// When the terminal cannot be written to.
    fn write_frame(&mut self, bytes: &[u8]) -> io::Result<()>;
}

impl Sink for Vec<u8> {
    fn write_frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// Where the cursor goes: a row of the live region and a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// The row, counted from the first row of the live region.
    pub row: usize,
    /// The column, counted from the left edge.
    pub col: usize,
}

/// What one frame shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame<'a> {
    /// Lines that can no longer change, in order. They go into the
    /// scrollback, and are never touched again.
    pub commit: &'a [Line],
    /// The live region, top to bottom.
    pub live: &'a [Line],
    /// Where the cursor is shown; `None` hides it.
    pub cursor: Option<Cursor>,
}

/// What [`Inline::draw`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawn {
    /// Nothing would change on screen, so nothing was written.
    Skipped,
    /// One frame of this many bytes was written.
    Written(usize),
}

/// One rendered row of the live region.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    bytes: Vec<u8>,
}

/// The inline presenter. See the [module documentation](self).
#[derive(Debug)]
pub struct Inline {
    cols: usize,
    rows: usize,
    /// The live region as it is on screen.
    live: Vec<Row>,
    /// The row of the live region the cursor was left on.
    cursor_row: usize,
    /// Where the cursor was shown, if it was.
    cursor: Option<Cursor>,
    /// What the renderer was told of the terminal for the rows on screen.
    target: Option<Target>,
    /// Whether the rows on screen can no longer be trusted (nothing drawn
    /// yet, a resize, a redraw asked for): the next frame writes them all
    /// and erases below.
    stale: bool,
    buffer: Vec<u8>,
    scratch: Vec<Row>,
}

impl Inline {
    /// A presenter for a terminal of `cols` by `rows`, drawing from the
    /// row the cursor is on.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: usize::from(cols.max(1)),
            rows: usize::from(rows.max(1)),
            live: Vec::new(),
            cursor_row: 0,
            cursor: None,
            target: None,
            stale: true,
            buffer: Vec::with_capacity(4096),
            scratch: Vec::new(),
        }
    }

    /// The terminal's size changed. The next frame redraws the whole live
    /// region at the new size.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (usize::from(cols.max(1)), usize::from(rows.max(1)));
        if (cols, rows) != (self.cols, self.rows) {
            self.cols = cols;
            self.rows = rows;
            self.stale = true;
        }
    }

    /// The next frame redraws the whole live region, as after `Ctrl+L`.
    pub fn redraw(&mut self) {
        self.stale = true;
    }

    /// Forgets the live region: the next frame starts on the row the cursor
    /// is on now, as after the program was suspended and resumed and the
    /// shell wrote below it.
    pub fn restart(&mut self) {
        self.live.clear();
        self.cursor_row = 0;
        self.cursor = None;
        self.stale = true;
    }

    /// The width the terminal has, in columns.
    #[must_use]
    pub fn cols(&self) -> u16 {
        u16::try_from(self.cols).unwrap_or(u16::MAX)
    }

    /// The height of the terminal, in rows.
    #[must_use]
    pub fn rows(&self) -> u16 {
        u16::try_from(self.rows).unwrap_or(u16::MAX)
    }

    /// How many rows the live region has on screen now.
    #[must_use]
    pub fn live_height(&self) -> usize {
        self.live.len()
    }

    /// The row of the live region the cursor is on now.
    #[must_use]
    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    /// Draws `frame` into `sink`, in one call, unless it would change
    /// nothing on screen.
    ///
    /// The live region is cut to the terminal's height, keeping its bottom
    /// rows: a region as tall as the screen could not be reached again once
    /// its top had scrolled away. Each line is cut to the terminal's width.
    ///
    /// # Errors
    ///
    /// When the sink cannot be written to. The presenter then assumes the
    /// frame did not arrive, and redraws everything next time.
    pub fn draw(
        &mut self,
        frame: &Frame<'_>,
        caps: &Capabilities,
        sink: &mut impl Sink,
    ) -> io::Result<Drawn> {
        let target = Target::of(caps);
        let skip = frame.live.len().saturating_sub(self.rows);
        let live = &frame.live[skip..];
        let cursor = frame.cursor.and_then(|cursor| {
            let row = cursor.row.checked_sub(skip)?;
            (row < live.len()).then_some(Cursor {
                row,
                col: cursor.col.min(self.line_width()),
            })
        });

        let mut rows = std::mem::take(&mut self.scratch);
        rows.clear();
        for line in live {
            let mut bytes = Vec::new();
            render::line(&mut bytes, line, self.line_width(), target);
            rows.push(Row { bytes });
        }

        if frame.commit.is_empty()
            && !self.stale
            && self.target == Some(target)
            && rows == self.live
            && cursor == self.cursor
        {
            self.scratch = rows;
            return Ok(Drawn::Skipped);
        }

        let mut out = std::mem::take(&mut self.buffer);
        out.clear();
        if caps.sync {
            out.extend_from_slice(SYNC_BEGIN);
        }
        out.extend_from_slice(HIDE_CURSOR);

        let full = self.stale
            || !frame.commit.is_empty()
            || rows.len() != self.live.len()
            || self.target != Some(target);
        let mut at = self.cursor_row;
        if full {
            // To the first row of the live region.
            out.push(b'\r');
            up(&mut out, at);
            for line in frame.commit {
                render::line(&mut out, line, self.line_width(), target);
                out.extend_from_slice(ERASE_LINE);
                out.extend_from_slice(b"\r\n");
            }
            for (index, row) in rows.iter().enumerate() {
                out.extend_from_slice(&row.bytes);
                out.extend_from_slice(ERASE_LINE);
                if index + 1 < rows.len() {
                    out.extend_from_slice(b"\r\n");
                }
            }
            // Rows of the old region left below the new one, or anything at
            // all after a resize. No row reaches the last column, so the
            // erase never takes a character of the row the cursor is on.
            if self.stale || self.live.len() > frame.commit.len() + rows.len() {
                out.extend_from_slice(ERASE_BELOW);
            }
            at = rows.len().saturating_sub(1);
        } else {
            for (index, (new, old)) in rows.iter().zip(&self.live).enumerate() {
                if new == old {
                    continue;
                }
                go(&mut out, at, index);
                out.push(b'\r');
                out.extend_from_slice(&new.bytes);
                out.extend_from_slice(ERASE_LINE);
                at = index;
            }
        }

        if let Some(cursor) = cursor {
            go(&mut out, at, cursor.row);
            out.push(b'\r');
            if cursor.col > 0 {
                push_csi(&mut out, cursor.col, b'C');
            }
            at = cursor.row;
            out.extend_from_slice(SHOW_CURSOR);
        }
        if caps.sync {
            out.extend_from_slice(SYNC_END);
        }

        let written = sink.write_frame(&out);
        let length = out.len();
        self.buffer = out;
        if let Err(error) = written {
            self.scratch = rows;
            self.stale = true;
            return Err(error);
        }
        self.scratch = std::mem::replace(&mut self.live, rows);
        self.cursor_row = at;
        self.cursor = cursor;
        self.target = Some(target);
        self.stale = false;
        Ok(Drawn::Written(length))
    }

    /// Erases the live region and leaves the cursor, shown, on its first
    /// row: what a program does before it exits, so the shell's prompt comes
    /// back where the region was.
    ///
    /// # Errors
    ///
    /// When the sink cannot be written to.
    pub fn finish(&mut self, caps: &Capabilities, sink: &mut impl Sink) -> io::Result<()> {
        let mut out = std::mem::take(&mut self.buffer);
        out.clear();
        if caps.sync {
            out.extend_from_slice(SYNC_BEGIN);
        }
        out.push(b'\r');
        up(&mut out, self.cursor_row);
        out.extend_from_slice(ERASE_BELOW);
        out.extend_from_slice(SHOW_CURSOR);
        if caps.sync {
            out.extend_from_slice(SYNC_END);
        }
        let written = sink.write_frame(&out);
        self.buffer = out;
        self.restart();
        written
    }

    /// The columns a line may use: all but the last. A row that filled the
    /// last column would leave the terminal waiting to wrap, which terminals
    /// handle differently (some wrap at once, and the next line break makes
    /// a blank row), and an erase after it would take its last character.
    #[must_use]
    pub fn width(&self) -> u16 {
        u16::try_from(self.line_width()).unwrap_or(u16::MAX)
    }

    fn line_width(&self) -> usize {
        self.cols.saturating_sub(1).max(1)
    }
}

fn push_csi(out: &mut Vec<u8>, count: usize, final_byte: u8) {
    out.extend_from_slice(b"\x1b[");
    if count != 1 {
        out.extend_from_slice(count.to_string().as_bytes());
    }
    out.push(final_byte);
}

/// Moves the cursor up `count` rows.
fn up(out: &mut Vec<u8>, count: usize) {
    if count > 0 {
        push_csi(out, count, b'A');
    }
}

/// Moves the cursor from row `from` to row `to` of the live region. Both
/// rows exist, so a move down never scrolls.
fn go(out: &mut Vec<u8>, from: usize, to: usize) {
    if to < from {
        push_csi(out, from - to, b'A');
    } else if to > from {
        push_csi(out, to - from, b'B');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{Overrides, Platform};

    fn caps() -> Capabilities {
        Capabilities::detect(|_| None, Platform::Unix, Overrides::default())
    }

    fn lines(texts: &[&str]) -> Vec<Line> {
        texts.iter().map(|text| Line::plain(*text)).collect()
    }

    fn draw(
        presenter: &mut Inline,
        commit: &[&str],
        live: &[&str],
        cursor: Option<Cursor>,
    ) -> (Drawn, String) {
        let commit = lines(commit);
        let live = lines(live);
        let mut sink = Vec::new();
        let drawn = presenter
            .draw(
                &Frame {
                    commit: &commit,
                    live: &live,
                    cursor,
                },
                &caps(),
                &mut sink,
            )
            .unwrap();
        (drawn, String::from_utf8(sink).unwrap())
    }

    #[test]
    fn the_first_frame_draws_everything_and_erases_below() {
        let mut presenter = Inline::new(20, 10);
        let (drawn, bytes) = draw(
            &mut presenter,
            &[],
            &["one", "two"],
            Some(Cursor { row: 1, col: 2 }),
        );
        assert_eq!(
            bytes,
            "\x1b[?2026h\x1b[?25l\rone\x1b[K\r\ntwo\x1b[K\x1b[J\r\x1b[2C\x1b[?25h\x1b[?2026l"
        );
        assert_eq!(drawn, Drawn::Written(bytes.len()));
        assert_eq!(presenter.cursor_row(), 1);
    }

    #[test]
    fn an_unchanged_frame_is_skipped_and_a_changed_row_is_written_alone() {
        let mut presenter = Inline::new(20, 10);
        let cursor = Some(Cursor { row: 2, col: 0 });
        draw(&mut presenter, &[], &["a", "b", "c"], cursor);
        assert_eq!(
            draw(&mut presenter, &[], &["a", "b", "c"], cursor),
            (Drawn::Skipped, String::new())
        );
        let (_, bytes) = draw(&mut presenter, &[], &["a", "B", "c"], cursor);
        assert_eq!(
            bytes,
            "\x1b[?2026h\x1b[?25l\x1b[A\rB\x1b[K\x1b[B\r\x1b[?25h\x1b[?2026l"
        );
    }

    #[test]
    fn commits_go_above_the_live_region() {
        let mut presenter = Inline::new(20, 10);
        draw(
            &mut presenter,
            &[],
            &["live 1", "live 2"],
            Some(Cursor { row: 1, col: 0 }),
        );
        let (_, bytes) = draw(&mut presenter, &["done"], &["live"], None);
        assert_eq!(
            bytes,
            "\x1b[?2026h\x1b[?25l\r\x1b[Adone\x1b[K\r\nlive\x1b[K\x1b[?2026l"
        );
        assert_eq!(presenter.live_height(), 1);
    }

    #[test]
    fn no_row_reaches_the_last_column() {
        let mut presenter = Inline::new(5, 10);
        assert_eq!(presenter.width(), 4);
        draw(&mut presenter, &[], &["a", "b", "c"], None);
        let (_, bytes) = draw(&mut presenter, &["0123456"], &["abcdef"], None);
        assert_eq!(
            bytes,
            "\x1b[?2026h\x1b[?25l\r\x1b[2A0123\x1b[K\r\nabcd\x1b[K\x1b[J\x1b[?2026l"
        );
    }

    #[test]
    fn an_empty_live_region_after_commits_erases_what_was_left() {
        let mut presenter = Inline::new(20, 10);
        draw(
            &mut presenter,
            &[],
            &["a", "b", "c"],
            Some(Cursor { row: 2, col: 0 }),
        );
        let (_, bytes) = draw(&mut presenter, &["x", "y"], &[], None);
        assert_eq!(
            bytes,
            "\x1b[?2026h\x1b[?25l\r\x1b[2Ax\x1b[K\r\ny\x1b[K\r\n\x1b[J\x1b[?2026l"
        );
    }

    #[test]
    fn a_region_taller_than_the_screen_keeps_its_bottom() {
        let mut presenter = Inline::new(10, 2);
        let (_, bytes) = draw(
            &mut presenter,
            &[],
            &["1", "2", "3"],
            Some(Cursor { row: 0, col: 0 }),
        );
        assert!(!bytes.contains('1'));
        assert!(bytes.contains("2\x1b[K\r\n3"));
        // The cursor was on a row that is no longer shown: it is hidden.
        assert!(!bytes.contains("\x1b[?25h"));
    }

    #[test]
    fn without_synchronized_updates_there_are_no_brackets() {
        let mut presenter = Inline::new(20, 10);
        let mut caps = caps();
        caps.sync = false;
        let live = lines(&["x"]);
        let mut sink = Vec::new();
        presenter
            .draw(
                &Frame {
                    commit: &[],
                    live: &live,
                    cursor: None,
                },
                &caps,
                &mut sink,
            )
            .unwrap();
        assert!(!String::from_utf8(sink).unwrap().contains("2026"));
    }

    #[test]
    fn finishing_erases_the_region_and_shows_the_cursor() {
        let mut presenter = Inline::new(20, 10);
        draw(
            &mut presenter,
            &[],
            &["a", "b"],
            Some(Cursor { row: 1, col: 0 }),
        );
        let mut sink = Vec::new();
        presenter.finish(&caps(), &mut sink).unwrap();
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            "\x1b[?2026h\r\x1b[A\x1b[J\x1b[?25h\x1b[?2026l"
        );
        assert_eq!(presenter.live_height(), 0);
    }

    #[test]
    fn a_failed_write_redraws_everything_next_time() {
        struct Broken;
        impl Sink for Broken {
            fn write_frame(&mut self, _: &[u8]) -> io::Result<()> {
                Err(io::Error::other("gone"))
            }
        }
        let mut presenter = Inline::new(20, 10);
        draw(&mut presenter, &[], &["a"], None);
        let live = lines(&["b"]);
        assert!(
            presenter
                .draw(
                    &Frame {
                        commit: &[],
                        live: &live,
                        cursor: None
                    },
                    &caps(),
                    &mut Broken
                )
                .is_err()
        );
        let (_, bytes) = draw(&mut presenter, &[], &["b"], None);
        assert!(bytes.contains("\x1b[J"), "{bytes:?}");
    }
}
