//! The styled-line model: what every component renders to
//! (`docs/architecture/09-tui.md`, "One renderer, two presenters").
//!
//! A [`Line`] is one row of the terminal: [`Span`]s of text, each with a
//! [`Style`] and perhaps a link. A line never holds a line break that means
//! one: the text of a span may hold any character, and the renderer shows
//! every control character as a visible symbol of one column
//! ([`crate::sanitize`]). So the width of a line is the width the terminal
//! gives it, and a line from a model or a tool cannot move the cursor.

use std::borrow::Cow;

use crate::style::Style;

/// A run of text in one style.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Span {
    text: String,
    style: Style,
    link: Option<String>,
}

impl Span {
    /// Text in a style.
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
        }
    }

    /// Text in the terminal's own style.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Style::PLAIN)
    }

    /// This span as a link to `url`, where the terminal can show links.
    ///
    /// Where it cannot, only the text is shown, so a component that needs
    /// the address seen puts it in the text as well. An address that holds a
    /// control character, or anything outside printable ASCII, is not used as
    /// a link at all: the text is shown alone.
    #[must_use]
    pub fn link(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        let safe = !url.is_empty() && url.bytes().all(|byte| (0x21..0x7f).contains(&byte));
        self.link = safe.then_some(url);
        self
    }

    /// The text, as given.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The style.
    #[must_use]
    pub fn style(&self) -> Style {
        self.style
    }

    /// The address this span links to, if it is a link.
    #[must_use]
    pub fn url(&self) -> Option<&str> {
        self.link.as_deref()
    }

    /// The width of the text in terminal columns, as the renderer shows it.
    #[must_use]
    pub fn width(&self) -> usize {
        cortex_base::text::width(&self.text)
    }
}

/// One row of the terminal: spans side by side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Line {
    spans: Vec<Span>,
}

impl Line {
    /// An empty line.
    #[must_use]
    pub const fn new() -> Self {
        Self { spans: Vec::new() }
    }

    /// A line of text in the terminal's own style.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::from(Span::plain(text))
    }

    /// A line of text in one style.
    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        Self::from(Span::new(text, style))
    }

    /// This line with `span` added at its end.
    #[must_use]
    pub fn with(mut self, span: Span) -> Self {
        self.push(span);
        self
    }

    /// Adds `span` at the end of the line.
    pub fn push(&mut self, span: Span) {
        if !span.text.is_empty() {
            self.spans.push(span);
        }
    }

    /// The spans, in order.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// The width of the line in terminal columns, as the renderer shows it.
    #[must_use]
    pub fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }

    /// The text of the line without its styles.
    #[must_use]
    pub fn text(&self) -> Cow<'_, str> {
        match self.spans.as_slice() {
            [] => Cow::Borrowed(""),
            [one] => Cow::Borrowed(&one.text),
            many => Cow::Owned(many.iter().map(|span| span.text.as_str()).collect()),
        }
    }
}

impl From<Span> for Line {
    fn from(span: Span) -> Self {
        let mut line = Self::new();
        line.push(span);
        line
    }
}

impl From<Vec<Span>> for Line {
    fn from(spans: Vec<Span>) -> Self {
        let mut line = Self::new();
        for span in spans {
            line.push(span);
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Color;

    #[test]
    fn a_line_is_as_wide_as_its_spans() {
        let line = Line::plain("ab").with(Span::new("漢字", Style::PLAIN.fg(Color::Ansi(2))));
        assert_eq!(line.width(), 6);
        assert_eq!(line.text(), "ab漢字");
        assert_eq!(Line::new().text(), "");
        // Empty spans are not kept.
        assert_eq!(Line::plain("").spans().len(), 0);
        // A control character is one column: the renderer shows a symbol.
        assert_eq!(Line::plain("a\u{1b}b").width(), 3);
    }

    #[test]
    fn only_a_printable_address_becomes_a_link() {
        assert_eq!(
            Span::plain("x").link("https://e.example/a?b=1").url(),
            Some("https://e.example/a?b=1")
        );
        for bad in [
            "",
            "https://e.example/\u{1b}]8;;",
            "a b",
            "https://e.example/\u{7}",
            "é",
        ] {
            assert_eq!(Span::plain("x").link(bad).url(), None, "{bad:?}");
        }
    }
}
