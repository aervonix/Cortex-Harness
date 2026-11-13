//! A text file as the editing tools see it: its bytes, checked to be UTF-8
//! text, with its byte-order mark and its line endings found and kept
//! (`docs/architecture/05-tools.md`, "Text files").
//!
//! The rule the editing tools keep is that **what they do not replace does
//! not change by a byte**. So nothing here converts a file: the content is
//! borrowed as it is, and the mark and the endings are described, to be
//! written back exactly.
//!
//! A line is what `read` shows as a line: the text up to a line feed,
//! without the carriage return of a CRLF ending. A carriage return that is
//! not followed by a line feed is part of its line. A final line break ends
//! the last line and does not start another.

use std::borrow::Cow;

use crate::detect::{self, Kind};

/// The byte-order mark, which some editors put at the start of a UTF-8
/// file.
pub(crate) const BOM: &str = "\u{feff}";

/// How a line ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// A line feed.
    Lf,
    /// A carriage return and a line feed.
    CrLf,
}

impl Ending {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// One line: where its content is in the text, and how it ends. The last
/// line of a file that does not end with a line break has no ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Line {
    /// The offset of its first byte.
    pub(crate) start: usize,
    /// The offset just past its content: where its ending begins.
    pub(crate) end: usize,
    pub(crate) ending: Option<Ending>,
}

impl Line {
    /// The offset just past the line and its ending.
    pub(crate) fn after(&self) -> usize {
        self.end + self.ending.map_or(0, |ending| ending.as_str().len())
    }
}

/// Why content is not text the editing tools change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotText {
    /// It is a binary file, of this kind.
    Binary(&'static str),
    /// It is not valid UTF-8. The first byte that is not is on this line.
    NotUtf8 { line: u64 },
}

/// Checks that `bytes` are UTF-8 text and takes the byte-order mark off:
/// whether there was one, and the text after it.
pub(crate) fn decode(bytes: &[u8]) -> Result<(bool, &str), NotText> {
    match detect::sniff(bytes) {
        Kind::Text => {}
        Kind::Image(_) => return Err(NotText::Binary("an image")),
        Kind::Binary(what) => return Err(NotText::Binary(what)),
    }
    let text = std::str::from_utf8(bytes).map_err(|error| NotText::NotUtf8 {
        line: 1 + bytes[..error.valid_up_to()]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u64,
    })?;
    // A NUL byte past what the sniff looked at: still not text.
    if text.contains('\0') {
        return Err(NotText::Binary("binary data"));
    }
    Ok(match text.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, text),
    })
}

/// The lines of a text, and what its line endings are.
#[derive(Debug, Clone)]
pub(crate) struct Text<'a> {
    /// The text, without a byte-order mark, exactly as the file has it.
    pub(crate) text: &'a str,
    pub(crate) lines: Vec<Line>,
    /// The ending most of its lines have: what a new line is given where
    /// the lines around it do not say.
    pub(crate) dominant: Ending,
    /// Whether any line ends with CRLF.
    pub(crate) has_crlf: bool,
}

impl<'a> Text<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let bytes = text.as_bytes();
        let mut lines = Vec::new();
        let mut start = 0;
        let (mut lf, mut crlf) = (0usize, 0usize);
        for (at, &byte) in bytes.iter().enumerate() {
            if byte != b'\n' {
                continue;
            }
            let ending = if at > start && bytes[at - 1] == b'\r' {
                crlf += 1;
                Ending::CrLf
            } else {
                lf += 1;
                Ending::Lf
            };
            lines.push(Line {
                start,
                end: at + 1 - ending.as_str().len(),
                ending: Some(ending),
            });
            start = at + 1;
        }
        if start < bytes.len() {
            lines.push(Line {
                start,
                end: bytes.len(),
                ending: None,
            });
        }
        let dominant = match crlf.cmp(&lf) {
            std::cmp::Ordering::Greater => Ending::CrLf,
            std::cmp::Ordering::Less => Ending::Lf,
            // As many of one as of the other: the first line decides.
            std::cmp::Ordering::Equal => lines
                .first()
                .and_then(|line| line.ending)
                .unwrap_or(Ending::Lf),
        };
        Self {
            text,
            lines,
            dominant,
            has_crlf: crlf > 0,
        }
    }

    /// The content of a line, without its ending.
    pub(crate) fn content(&self, line: usize) -> &'a str {
        let line = &self.lines[line];
        &self.text[line.start..line.end]
    }

    /// The index of the line that holds the byte at `offset`, counting a
    /// line's ending as part of it. For an offset at the end of the text,
    /// the last line.
    pub(crate) fn line_at(&self, offset: usize) -> usize {
        self.lines
            .partition_point(|line| line.start <= offset)
            .saturating_sub(1)
    }

    /// The ending a line break gets when it is written at `offset`: that of
    /// the line there, and where that line has none (the last line of a
    /// file that does not end with a line break), the file's own.
    pub(crate) fn ending_at(&self, offset: usize) -> Ending {
        self.lines
            .get(self.line_at(offset))
            .and_then(|line| line.ending)
            .unwrap_or(self.dominant)
    }

    /// The text as `read` shows it: every CRLF read as one line break.
    pub(crate) fn view(&self) -> View<'a> {
        if !self.has_crlf {
            return View {
                text: Cow::Borrowed(self.text),
                marks: None,
            };
        }
        let mut view = String::with_capacity(self.text.len());
        let mut marks = Vec::with_capacity(self.lines.len() + 1);
        for line in &self.lines {
            marks.push((view.len(), line.start));
            view.push_str(&self.text[line.start..line.end]);
            if line.ending.is_some() {
                view.push('\n');
            }
        }
        marks.push((view.len(), self.text.len()));
        View {
            text: Cow::Owned(view),
            marks: Some(marks),
        }
    }
}

/// A text with its line breaks all written as a line feed, and the way back
/// from an offset in it to the offset in the file's own text.
#[derive(Debug, Clone)]
pub(crate) struct View<'a> {
    pub(crate) text: Cow<'a, str>,
    /// For each line, where it starts in the view and where in the file,
    /// and one more pair for the end of both. `None` when the two are the
    /// same text.
    marks: Option<Vec<(usize, usize)>>,
}

impl View<'_> {
    /// The offset in the file's text of an offset in the view. A line break
    /// in the view is where the file's line ending begins.
    pub(crate) fn to_file(&self, offset: usize) -> usize {
        let Some(marks) = &self.marks else {
            return offset;
        };
        let at = marks
            .partition_point(|&(view, _)| view <= offset)
            .saturating_sub(1);
        let (view, file) = marks[at];
        file + (offset - view)
    }
}

/// The text of `new` as it is written into a file: its line breaks, however
/// the model wrote them, as `ending`.
pub(crate) fn with_ending(new: &str, ending: Ending) -> String {
    let mut written = String::with_capacity(new.len() + 8);
    let mut rest = new;
    while let Some(at) = rest.find('\n') {
        let line = &rest[..at];
        written.push_str(line.strip_suffix('\r').unwrap_or(line));
        written.push_str(ending.as_str());
        rest = &rest[at + 1..];
    }
    written.push_str(rest);
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines<'a>(text: &Text<'a>) -> Vec<(&'a str, Option<Ending>)> {
        (0..text.lines.len())
            .map(|line| (text.content(line), text.lines[line].ending))
            .collect()
    }

    #[test]
    fn lines_are_what_read_shows_and_keep_their_endings() {
        let text = Text::new("one\r\ntwo\nthree\r\n\r\nlast");
        assert_eq!(
            lines(&text),
            [
                ("one", Some(Ending::CrLf)),
                ("two", Some(Ending::Lf)),
                ("three", Some(Ending::CrLf)),
                ("", Some(Ending::CrLf)),
                ("last", None),
            ]
        );
        assert_eq!(text.dominant, Ending::CrLf);
        assert!(text.has_crlf);

        // A final line break ends the last line and starts no other.
        let text = Text::new("a\nb\n");
        assert_eq!(
            lines(&text),
            [("a", Some(Ending::Lf)), ("b", Some(Ending::Lf))]
        );
        assert_eq!(text.dominant, Ending::Lf);
        assert!(!text.has_crlf);
        assert!(Text::new("").lines.is_empty());
        assert_eq!(lines(&Text::new("\n")), [("", Some(Ending::Lf))]);
        // A carriage return alone is content, not an ending.
        assert_eq!(
            lines(&Text::new("a\rb\n\r")),
            [("a\rb", Some(Ending::Lf)), ("\r", None)]
        );
    }

    #[test]
    fn the_first_line_decides_between_as_many_endings_of_each_kind() {
        assert_eq!(Text::new("a\r\nb\n").dominant, Ending::CrLf);
        assert_eq!(Text::new("a\nb\r\n").dominant, Ending::Lf);
        assert_eq!(Text::new("no line break").dominant, Ending::Lf);
        assert_eq!(Text::new("a\nb\r\nc\r\n").dominant, Ending::CrLf);
    }

    #[test]
    fn an_offset_names_its_line_and_the_ending_to_write_there() {
        let text = Text::new("ab\r\ncd\nef");
        //                    0123 4 567 8 9
        assert_eq!(text.line_at(0), 0);
        assert_eq!(text.line_at(2), 0, "the ending belongs to its line");
        assert_eq!(text.line_at(3), 0);
        assert_eq!(text.line_at(4), 1);
        assert_eq!(text.line_at(7), 2);
        assert_eq!(text.line_at(9), 2, "the end of the text is its last line");
        assert_eq!(text.ending_at(1), Ending::CrLf);
        assert_eq!(text.ending_at(5), Ending::Lf);
        // The last line has no ending of its own: the file's is used, and
        // with one of each the first line decides.
        assert_eq!(text.ending_at(8), Ending::CrLf);
        assert_eq!(Text::new("").line_at(0), 0);
        assert_eq!(Text::new("").ending_at(0), Ending::Lf);
        assert_eq!(text.lines[0].after(), 4);
        assert_eq!(text.lines[2].after(), 9);
    }

    #[test]
    fn the_view_reads_crlf_as_one_line_break_and_finds_its_way_back() {
        let text = Text::new("ab\r\ncd\nef\r\n");
        let view = text.view();
        assert_eq!(view.text, "ab\ncd\nef\n");
        // Every offset of the view, back in the file.
        let expected = [0, 1, 2, 4, 5, 6, 7, 8, 9, 11];
        for (offset, file) in expected.into_iter().enumerate() {
            assert_eq!(view.to_file(offset), file, "view offset {offset}");
        }
        // With no CRLF the view is the text itself.
        let plain = Text::new("ab\ncd");
        let view = plain.view();
        assert!(matches!(view.text, Cow::Borrowed(_)));
        assert_eq!(view.to_file(4), 4);
    }

    #[test]
    fn decoding_takes_the_mark_off_and_refuses_what_is_not_text() {
        assert_eq!(decode(b"plain\n"), Ok((false, "plain\n")));
        assert_eq!(
            decode("\u{feff}marked\r\n".as_bytes()),
            Ok((true, "marked\r\n"))
        );
        assert_eq!(decode(b""), Ok((false, "")));
        assert_eq!(
            decode(b"one\ntwo\nbad \xff byte\n"),
            Err(NotText::NotUtf8 { line: 3 })
        );
        assert_eq!(decode(b"\xc3"), Err(NotText::NotUtf8 { line: 1 }));
        assert_eq!(
            decode(b"\x89PNG\r\n\x1a\n\0\0"),
            Err(NotText::Binary("an image"))
        );
        assert_eq!(
            decode(b"\x7fELF\x02\x01\x01\0"),
            Err(NotText::Binary("an ELF executable"))
        );
        // A NUL byte far past the start is still not text.
        let mut late = vec![b'a'; detect::SNIFF_BYTES + 10];
        late.push(0);
        assert_eq!(decode(&late), Err(NotText::Binary("binary data")));
    }

    #[test]
    fn new_text_is_written_with_the_ending_it_is_given() {
        assert_eq!(with_ending("a\nb\n", Ending::CrLf), "a\r\nb\r\n");
        assert_eq!(with_ending("a\r\nb\nc", Ending::CrLf), "a\r\nb\r\nc");
        assert_eq!(with_ending("a\r\nb\r\n", Ending::Lf), "a\nb\n");
        assert_eq!(with_ending("no break", Ending::CrLf), "no break");
        assert_eq!(with_ending("", Ending::CrLf), "");
        // A carriage return that is not part of a line break stays.
        assert_eq!(with_ending("a\rb\n", Ending::Lf), "a\rb\n");
    }
}
