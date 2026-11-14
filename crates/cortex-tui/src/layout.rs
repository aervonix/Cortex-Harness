//! Laying out styled text at a width: wrapping, cutting with an ellipsis,
//! padding, and a row with a left and a right side.
//!
//! Everything here works on grapheme clusters and terminal columns, through
//! `cortex_base::text`, so a cluster (an accented letter, a flag, a family
//! emoji) is never split, and a line is never wider than it was given room
//! for, except where one cluster alone is wider than a whole line (a wide
//! character on a line of one column), which then has a line to itself.
//! Views lay out with these and nothing else, which is what keeps every
//! row out of the terminal's last column: the width a view is given is
//! already one less than the window's (`Inline::width`).
//!
//! These do not sanitise. Content from a model, a tool or a file goes
//! through `cortex_term::sanitize` before it becomes a [`Span`].

use cortex_base::text::{graphemes, width as text_width, wrap_ranges};
use cortex_term::{Line, Span, Style};

/// The text of `spans` wrapped into lines of at most `width` columns, each
/// span keeping its style (and link) on every line it reaches.
///
/// The rules are `cortex_base::text::wrap_ranges`'s, on the spans' text read
/// as one: a line breaks at white space, which is dropped at the break; a
/// line break in the text always ends a line; a word wider than a line is
/// broken between grapheme clusters. Empty text is one empty line. A
/// `width` of 0 is taken as 1, and a cluster wider than `width` has a line
/// to itself.
#[must_use]
pub fn wrap(spans: &[Span], width: usize) -> Vec<Line> {
    let mut text = String::new();
    // Where each span starts in `text`.
    let mut starts = Vec::with_capacity(spans.len());
    for span in spans {
        starts.push(text.len());
        text.push_str(span.text());
    }
    wrap_ranges(&text, width)
        .into_iter()
        .map(|range| {
            let mut line = Line::new();
            for (span, &start) in spans.iter().zip(&starts) {
                let end = start + span.text().len();
                let (from, to) = (range.start.max(start), range.end.min(end));
                if from < to {
                    line.push(with_text(span, &text[from..to]));
                }
            }
            line
        })
        .collect()
}

/// `line` cut to at most `width` columns, keeping its beginning and ending
/// in `ellipsis` when anything was cut. A line that fits is returned whole.
///
/// The cut is between grapheme clusters, so the result may be a column
/// narrower than `width` where a wide character did not fit. The ellipsis
/// takes the style of the text just before it. If `width` has no room for
/// the ellipsis, the result is as much of the ellipsis as fits.
#[must_use]
pub fn cut(line: &Line, width: usize, ellipsis: &str) -> Line {
    if line.width() <= width {
        return line.clone();
    }
    let mark_width = text_width(ellipsis);
    if mark_width >= width {
        let style = line.spans().first().map_or(Style::PLAIN, Span::style);
        return Line::from(Span::new(prefix(ellipsis, width), style));
    }
    let room = width - mark_width;
    let mut out = Line::new();
    let mut used = 0;
    let mut mark_style = line.spans().first().map_or(Style::PLAIN, Span::style);
    for span in line.spans() {
        let kept = prefix(span.text(), room - used);
        used += text_width(kept);
        if !kept.is_empty() {
            mark_style = span.style();
            out.push(with_text(span, kept));
        }
        if kept.len() < span.text().len() {
            break;
        }
    }
    out.push(Span::new(ellipsis, mark_style));
    out
}

/// `line` with spaces in `style` added at its end, to make it `width`
/// columns wide. A line as wide or wider is left as it is.
#[must_use]
pub fn pad(mut line: Line, width: usize, style: Style) -> Line {
    let have = line.width();
    if have < width {
        line.push(Span::new(" ".repeat(width - have), style));
    }
    line
}

/// One row of `width` columns with `left` at its start and `right` at its
/// end, at least `gap` columns apart. When both do not fit, `left` is cut
/// (with `ellipsis`) to make room; when `right` alone does not fit, it is
/// dropped and `left` has the row.
#[must_use]
pub fn spread(left: &Line, right: &Line, width: usize, gap: usize, ellipsis: &str) -> Line {
    let right_width = right.width();
    if right_width + gap > width {
        return cut(left, width, ellipsis);
    }
    let left = cut(left, width - right_width - gap, ellipsis);
    let mut row = pad(left, width - right_width, Style::PLAIN);
    for span in right.spans() {
        row.push(span.clone());
    }
    row
}

/// A line made of `spans`, in order: the same as `Line::from`, for slices.
#[must_use]
pub fn join(spans: &[Span]) -> Line {
    let mut line = Line::new();
    for span in spans {
        line.push(span.clone());
    }
    line
}

/// The spans of `first` and then those of `second`, as one line.
#[must_use]
pub fn concat(first: &Line, second: &Line) -> Line {
    let mut line = first.clone();
    for span in second.spans() {
        line.push(span.clone());
    }
    line
}

/// `span` with other text: the same style and link.
fn with_text(span: &Span, text: &str) -> Span {
    let new = Span::new(text, span.style());
    match span.url() {
        Some(url) => new.link(url),
        None => new,
    }
}

/// The longest beginning of `text` at most `width` columns wide, ending
/// between grapheme clusters.
fn prefix(text: &str, width: usize) -> &str {
    let mut used = 0;
    for (offset, _, columns) in graphemes(text) {
        if used + columns > width {
            return &text[..offset];
        }
        used += columns;
    }
    text
}

#[cfg(test)]
mod tests;
