//! The rounded box: a border around rows of text, with an optional label set
//! into the bottom border (`docs/architecture/09-tui.md`, "Tool rounds" and
//! "The live region").
//!
//! The session header, each tool round and the input box are drawn in one.
//!
//! ```text
//! ╭──────────────────────────────╮
//! │ ✓ Read lex.rs  lines 120–178 │
//! ╰─────────────── ctrl+t expands ─╯
//! ```
//!
//! A box is exactly as wide as it is told, so a box drawn at a view's width
//! leaves the terminal's last column empty like every other row. A row's
//! text sits after the left border and a space, and is cut with the glyph
//! set's ellipsis to [`RoundedBox::inner`] columns; the rest of the row up to
//! the space before the right border is filled in the row's own style, which
//! is how a diff line's tint reaches the box's edge.

use cortex_term::{Line, Span, Style};

use crate::glyphs::Glyphs;
use crate::layout::{cut, pad};

/// A rounded box of one width. See the [module documentation](self).
#[derive(Debug, Clone, Copy)]
pub struct RoundedBox<'a> {
    width: usize,
    glyphs: &'a Glyphs,
    border: Style,
}

impl<'a> RoundedBox<'a> {
    /// A box `width` columns wide, its border drawn in `border` with the
    /// corners and edges of `glyphs`.
    #[must_use]
    pub fn new(width: usize, glyphs: &'a Glyphs, border: Style) -> Self {
        Self {
            width,
            glyphs,
            border,
        }
    }

    /// The columns a row's text has: the width less the two borders and the
    /// space inside each.
    #[must_use]
    pub fn inner(&self) -> usize {
        self.width.saturating_sub(4)
    }

    /// The top border.
    #[must_use]
    pub fn top(&self) -> Line {
        self.edge(self.glyphs.corner_top_left, self.glyphs.corner_top_right)
    }

    /// One row: `text` cut to [`inner`](Self::inner) columns, then filled to
    /// them with spaces in `fill`.
    #[must_use]
    pub fn row(&self, text: &Line, fill: Style) -> Line {
        let side = Span::new(self.glyphs.vertical, self.border);
        let mut line = Line::from(side.clone()).with(Span::plain(" "));
        let body = pad(
            cut(text, self.inner(), self.glyphs.ellipsis),
            self.inner(),
            fill,
        );
        for span in body.spans() {
            line.push(span.clone());
        }
        let line = line.with(Span::plain(" ")).with(side);
        self.fit(line)
    }

    /// The bottom border, with `label` set into it near its right end
    /// (`╰──── ctrl+t expands ─╯`) when there is one. A label too long for the
    /// border is cut; with no room for any of it, it is left out.
    #[must_use]
    pub fn bottom(&self, label: Option<&Line>) -> Line {
        let g = self.glyphs;
        let Some(label) = label.filter(|label| label.width() > 0) else {
            return self.edge(g.corner_bottom_left, g.corner_bottom_right);
        };
        // The corner, the rule, a space, the label, a space, one stroke of
        // the rule and the corner.
        let room = self.width.saturating_sub(5);
        if room < 2 {
            return self.edge(g.corner_bottom_left, g.corner_bottom_right);
        }
        let label = cut(label, room, g.ellipsis);
        let rule = self.width - 5 - label.width();
        let mut line = Line::from(Span::new(
            format!("{}{}", g.corner_bottom_left, g.horizontal.repeat(rule)),
            self.border,
        ))
        .with(Span::new(" ", self.border));
        for span in label.spans() {
            line.push(span.clone());
        }
        line.push(Span::new(" ", self.border));
        line.push(Span::new(
            format!("{}{}", g.horizontal, g.corner_bottom_right),
            self.border,
        ));
        self.fit(line)
    }

    /// The whole box: the top border, a row for each of `rows` (its text and
    /// fill), and the bottom border with `label`.
    #[must_use]
    pub fn draw(&self, rows: &[(Line, Style)], label: Option<&Line>) -> Vec<Line> {
        let mut lines = Vec::with_capacity(rows.len() + 2);
        lines.push(self.top());
        for (text, fill) in rows {
            lines.push(self.row(text, *fill));
        }
        lines.push(self.bottom(label));
        lines
    }

    fn edge(&self, left: &str, right: &str) -> Line {
        let rule = self.glyphs.horizontal.repeat(self.width.saturating_sub(2));
        self.fit(Line::styled(format!("{left}{rule}{right}"), self.border))
    }

    /// A row of a box too narrow for its borders is cut to the width, so
    /// that nothing ever passes it.
    fn fit(&self, line: Line) -> Line {
        if line.width() <= self.width {
            line
        } else {
            cut(&line, self.width, "")
        }
    }
}

#[cfg(test)]
mod tests;
