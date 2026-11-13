//! The line renderer: a [`Line`] as the bytes that draw it on one row.
//!
//! It writes text, style changes and links, and nothing else: no cursor
//! movement, no erasing. Those are the presenter's. What it promises:
//!
//! - **Nothing from content reaches the terminal as a control character.**
//!   Every character goes through [`visible`](crate::sanitize::visible); a
//!   line feed or a tab in a span is shown as a symbol, one column wide.
//! - **A line never wraps.** It is cut at the width it is given, between
//!   grapheme clusters, so a line is always one row and the presenter's count
//!   of rows is right. A wide character that does not fit whole is left out.
//! - **A line ends in the terminal's own style,** with no link open, so
//!   whatever the presenter writes next (an erase, the next line) is not
//!   coloured by it.

use std::io::Write as _;

use crate::caps::Capabilities;
use crate::line::Line;
use crate::sanitize::visible;
use crate::style::{Color, ColorDepth, Style};

/// What the renderer needs to know of the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) depth: ColorDepth,
    pub(crate) links: bool,
}

impl Target {
    pub(crate) fn of(caps: &Capabilities) -> Self {
        Self {
            depth: caps.color,
            links: caps.hyperlinks,
        }
    }
}

/// Writes `line` into `out`, cut to `width` columns, and returns the
/// columns it took.
pub(crate) fn line(out: &mut Vec<u8>, line: &Line, width: usize, target: Target) -> usize {
    let mut used = 0;
    let mut current = Style::PLAIN;
    let mut link: Option<&str> = None;
    'spans: for span in line.spans() {
        let style = span.style().for_depth(target.depth);
        let url = if target.links { span.url() } else { None };
        let mut started = false;
        for (_, grapheme, columns) in cortex_base::text::graphemes(span.text()) {
            if used + columns > width {
                break 'spans;
            }
            if !started {
                // Style and link change only when there is text to show in
                // them.
                started = true;
                if url != link {
                    if link.is_some() {
                        out.extend_from_slice(b"\x1b]8;;\x1b\\");
                    }
                    if let Some(url) = url {
                        out.extend_from_slice(b"\x1b]8;;");
                        out.extend_from_slice(url.as_bytes());
                        out.extend_from_slice(b"\x1b\\");
                    }
                    link = url;
                }
                sgr(out, current, style);
                current = style;
            }
            push_grapheme(out, grapheme);
            used += columns;
        }
    }
    if link.is_some() {
        out.extend_from_slice(b"\x1b]8;;\x1b\\");
    }
    sgr(out, current, Style::PLAIN);
    used
}

/// Writes one grapheme cluster with every control character made visible.
fn push_grapheme(out: &mut Vec<u8>, grapheme: &str) {
    if grapheme.len() == 1 && (0x20..0x7f).contains(&grapheme.as_bytes()[0]) {
        out.push(grapheme.as_bytes()[0]);
        return;
    }
    let mut buffer = [0; 4];
    for c in grapheme.chars() {
        let shown = visible(c).unwrap_or(c);
        out.extend_from_slice(shown.encode_utf8(&mut buffer).as_bytes());
    }
}

/// Writes the shortest change of style from `from` to `to`: nothing when
/// they are the same, the attributes and colours that changed when only
/// additions are needed, and a reset followed by the whole of `to` when an
/// attribute is turned off.
pub(crate) fn sgr(out: &mut Vec<u8>, from: Style, to: Style) {
    if from == to {
        return;
    }
    let turned_off = (from.bold && !to.bold)
        || (from.dim && !to.dim)
        || (from.italic && !to.italic)
        || (from.underline && !to.underline)
        || (from.reverse && !to.reverse);
    let from = if turned_off {
        if to == Style::PLAIN {
            out.extend_from_slice(b"\x1b[0m");
            return;
        }
        out.extend_from_slice(b"\x1b[0;");
        Style::PLAIN
    } else {
        out.extend_from_slice(b"\x1b[");
        from
    };
    let mut first = true;
    let mut param = |out: &mut Vec<u8>, text: std::fmt::Arguments<'_>| {
        if !first {
            out.push(b';');
        }
        first = false;
        let _ = out.write_fmt(text);
    };
    if to.bold && !from.bold {
        param(out, format_args!("1"));
    }
    if to.dim && !from.dim {
        param(out, format_args!("2"));
    }
    if to.italic && !from.italic {
        param(out, format_args!("3"));
    }
    if to.underline && !from.underline {
        param(out, format_args!("4"));
    }
    if to.reverse && !from.reverse {
        param(out, format_args!("7"));
    }
    if to.fg != from.fg {
        match to.fg {
            Color::Default => param(out, format_args!("39")),
            Color::Ansi(n @ 0..=7) => param(out, format_args!("{}", 30 + n)),
            Color::Ansi(n) => param(out, format_args!("{}", 90 + (n % 16) - 8)),
            Color::Indexed(n) => param(out, format_args!("38;5;{n}")),
            Color::Rgb(r, g, b) => param(out, format_args!("38;2;{r};{g};{b}")),
        }
    }
    if to.bg != from.bg {
        match to.bg {
            Color::Default => param(out, format_args!("49")),
            Color::Ansi(n @ 0..=7) => param(out, format_args!("{}", 40 + n)),
            Color::Ansi(n) => param(out, format_args!("{}", 100 + (n % 16) - 8)),
            Color::Indexed(n) => param(out, format_args!("48;5;{n}")),
            Color::Rgb(r, g, b) => param(out, format_args!("48;2;{r};{g};{b}")),
        }
    }
    out.push(b'm');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::Span;

    const FULL: Target = Target {
        depth: ColorDepth::TrueColor,
        links: true,
    };

    fn render(line: &Line, width: usize, target: Target) -> (String, usize) {
        let mut out = Vec::new();
        let used = super::line(&mut out, line, width, target);
        (String::from_utf8(out).unwrap(), used)
    }

    #[test]
    fn plain_text_is_written_as_it_is() {
        assert_eq!(
            render(&Line::plain("hello"), 80, FULL),
            ("hello".to_owned(), 5)
        );
        assert_eq!(render(&Line::new(), 80, FULL), (String::new(), 0));
    }

    #[test]
    fn styles_change_only_where_they_differ_and_end_plain() {
        let red = Style::PLAIN.fg(Color::Ansi(1));
        let line = Line::plain("a")
            .with(Span::new("b", red))
            .with(Span::new("c", red.bold()))
            .with(Span::new("d", red))
            .with(Span::new("e", Style::PLAIN.bg(Color::Rgb(1, 2, 3))));
        let (text, used) = render(&line, 80, FULL);
        assert_eq!(
            text,
            "a\x1b[31mb\x1b[1mc\x1b[0;31md\x1b[39;48;2;1;2;3me\x1b[49m"
        );
        assert_eq!(used, 5);
        let bright = Line::styled("x", Style::PLAIN.fg(Color::Ansi(9)).bg(Color::Ansi(12)));
        assert_eq!(
            render(&bright, 80, FULL).0,
            "\x1b[91;104mx\x1b[0m".replace("\x1b[0m", "\x1b[39;49m")
        );
        let indexed = Line::styled("x", Style::PLAIN.fg(Color::Indexed(99)).underline());
        assert_eq!(render(&indexed, 80, FULL).0, "\x1b[4;38;5;99mx\x1b[0m");
    }

    #[test]
    fn colours_follow_the_terminals_depth() {
        let line = Line::styled("x", Style::PLAIN.fg(Color::Rgb(255, 0, 0)));
        let mono = Target {
            depth: ColorDepth::Mono,
            links: false,
        };
        assert_eq!(render(&line, 80, mono).0, "x");
        let ansi = Target {
            depth: ColorDepth::Ansi16,
            links: false,
        };
        assert_eq!(render(&line, 80, ansi).0, "\x1b[91mx\x1b[39m");
    }

    #[test]
    fn a_line_is_cut_at_its_width_and_never_wraps() {
        assert_eq!(
            render(&Line::plain("abcdef"), 4, FULL),
            ("abcd".to_owned(), 4)
        );
        // A wide character that does not fit whole is left out.
        assert_eq!(
            render(&Line::plain("ab漢字"), 5, FULL),
            ("ab漢".to_owned(), 4)
        );
        // A combining mark stays with its letter.
        assert_eq!(
            render(&Line::plain("ae\u{301}x"), 2, FULL),
            ("ae\u{301}".to_owned(), 2)
        );
        // The style is closed even when the cut falls inside a span.
        let line = Line::styled("abcdef", Style::PLAIN.bold());
        assert_eq!(render(&line, 3, FULL).0, "\x1b[1mabc\x1b[0m");
        assert_eq!(render(&line, 0, FULL), (String::new(), 0));
    }

    #[test]
    fn no_control_character_from_content_reaches_the_terminal() {
        let hostile = "a\u{1b}[2J\u{1b}]0;title\u{7}\u{9b}31m\r\n\tb\u{7f}\u{0}";
        let (text, used) = render(&Line::plain(hostile), 200, FULL);
        assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
        assert_eq!(used, cortex_base::text::width(hostile));
        assert_eq!(
            text,
            "a\u{241b}[2J\u{241b}]0;title\u{2407}\u{fffd}31m\u{240d}\u{240a}\u{2409}b\u{2421}\u{2400}"
        );
    }

    #[test]
    fn a_link_is_written_where_the_terminal_shows_links() {
        let line = Line::plain("see ")
            .with(Span::plain("docs").link("https://e.example/d"))
            .with(Span::plain("."));
        assert_eq!(
            render(&line, 80, FULL).0,
            "see \x1b]8;;https://e.example/d\x1b\\docs\x1b]8;;\x1b\\."
        );
        let no_links = Target {
            depth: ColorDepth::TrueColor,
            links: false,
        };
        assert_eq!(render(&line, 80, no_links).0, "see docs.");
        // A link cut short is still closed.
        assert_eq!(
            render(&line, 6, FULL).0,
            "see \x1b]8;;https://e.example/d\x1b\\do\x1b]8;;\x1b\\"
        );
    }
}
