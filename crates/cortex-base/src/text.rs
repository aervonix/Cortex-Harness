//! Text as a terminal shows it: how wide it is, where it wraps, and how to
//! cut it short.
//!
//! Everything here works on **grapheme clusters**, what a reader sees as one
//! character: `e` with a combining accent, a flag made of two code points, a
//! family emoji made of five. A cluster is never split, so a cut or a wrap
//! never leaves half a character behind.
//!
//! **Width** is counted in terminal columns: most characters take one,
//! East Asian wide characters and emoji take two, and combining marks take
//! none. A control character counts as one column, because the terminal layer
//! shows each one as a visible symbol and never sends it raw
//! (`docs/architecture/09-tui.md`, "Sanitising"). A tab therefore counts as
//! one column too: where real tab stops matter, call [`expand_tabs`] first.
//!
//! Characters whose width depends on the terminal's settings ("ambiguous"
//! ones, such as `§` or `→`) are counted as narrow.

use std::borrow::Cow;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The mark that stands for text that was cut: one column wide.
pub const ELLIPSIS: &str = "…";

/// The width of `text` in terminal columns.
///
/// It is the sum of the widths of its grapheme clusters, so a line break in
/// the text counts like any other control character. Measure one line at a
/// time.
#[must_use]
pub fn width(text: &str) -> usize {
    if text.is_ascii() {
        // Every ASCII character is one column, control characters included.
        return text.len();
    }
    text.graphemes(true).map(grapheme_width).sum()
}

/// The width of one grapheme cluster in terminal columns: 0, 1 or 2 for
/// nearly everything.
#[must_use]
pub fn grapheme_width(grapheme: &str) -> usize {
    let mut chars = grapheme.chars();
    match (chars.next(), chars.next()) {
        (None, _) => 0,
        (Some(only), None) if only.is_ascii() => 1,
        // A lone control character, or the pair `\r\n`: one column each.
        _ if grapheme.chars().all(char::is_control) => grapheme.chars().count(),
        _ => UnicodeWidthStr::width(grapheme),
    }
}

/// The grapheme clusters of `text`, each with the byte offset it starts at
/// and its width.
pub fn graphemes(text: &str) -> impl Iterator<Item = (usize, &str, usize)> {
    text.grapheme_indices(true)
        .map(|(offset, grapheme)| (offset, grapheme, grapheme_width(grapheme)))
}

/// Which part of a text survives when it is cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keep {
    /// The beginning: `a long sente…`. For prose and messages.
    Start,
    /// The end: `…ong sentence`. For text whose end matters most, such as a
    /// command's last line.
    End,
    /// Both ends: `a long…ntence`. For paths and identifiers.
    Ends,
}

/// Cuts `text` to at most `max_width` columns, keeping its beginning and
/// ending it with [`ELLIPSIS`]. Text that fits is returned as it is.
#[must_use]
pub fn truncate(text: &str, max_width: usize) -> Cow<'_, str> {
    truncate_with(text, max_width, ELLIPSIS, Keep::Start)
}

/// Cuts `text` to at most `max_width` columns, putting `marker` where text
/// was removed.
///
/// - Text that fits is returned as it is, borrowed.
/// - The result is never wider than `max_width`, marker included.
/// - The cut is always between grapheme clusters. Because a wide character
///   cannot be halved, the result may be a column narrower than `max_width`.
/// - If `max_width` has no room for the marker, the result is the marker's
///   own beginning: as much of it as fits, and none of the text.
#[must_use]
pub fn truncate_with<'a>(
    text: &'a str,
    max_width: usize,
    marker: &str,
    keep: Keep,
) -> Cow<'a, str> {
    if fits(text, max_width) {
        return Cow::Borrowed(text);
    }
    let marker_width = width(marker);
    if marker_width >= max_width {
        return Cow::Owned(prefix_within(marker, max_width).to_owned());
    }
    let room = max_width - marker_width;
    let (head, tail) = match keep {
        Keep::Start => (prefix_within(text, room), ""),
        Keep::End => ("", suffix_within(text, room)),
        Keep::Ends => {
            // The beginning gets the odd column.
            let head = prefix_within(text, room.div_ceil(2));
            let tail = suffix_within(&text[head.len()..], room - width(head));
            (head, tail)
        }
    };
    let mut out = String::with_capacity(head.len() + marker.len() + tail.len());
    out.push_str(head);
    out.push_str(marker);
    out.push_str(tail);
    Cow::Owned(out)
}

/// The longest beginning of `text` that is at most `max_bytes` long and ends
/// between grapheme clusters. For limits that are counted in bytes, such as
/// how much of a body goes to the log.
#[must_use]
pub fn truncate_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if offset + grapheme.len() > max_bytes {
            break;
        }
        end = offset + grapheme.len();
    }
    &text[..end]
}

/// Whether `text` is at most `max_width` columns wide, without measuring
/// more of a long text than needed.
fn fits(text: &str, max_width: usize) -> bool {
    // No grapheme is narrower than zero or, per byte, wider than one column
    // (the widest are two columns in three bytes).
    if text.len() <= max_width {
        return true;
    }
    let mut total = 0;
    for (_, _, columns) in graphemes(text) {
        total += columns;
        if total > max_width {
            return false;
        }
    }
    true
}

/// The longest beginning of `text` that is at most `max_width` columns wide.
fn prefix_within(text: &str, max_width: usize) -> &str {
    let mut total = 0;
    for (offset, _, columns) in graphemes(text) {
        if total + columns > max_width {
            return &text[..offset];
        }
        total += columns;
    }
    text
}

/// The longest end of `text` that is at most `max_width` columns wide.
fn suffix_within(text: &str, max_width: usize) -> &str {
    let mut total = 0;
    for (offset, grapheme) in text.grapheme_indices(true).rev() {
        let columns = grapheme_width(grapheme);
        if total + columns > max_width {
            return &text[offset + grapheme.len()..];
        }
        total += columns;
    }
    text
}

/// Replaces each tab with spaces up to the next multiple of `tab_stop`
/// columns, counting from the start of each line. Text with no tab is
/// returned as it is. A `tab_stop` of 0 removes tabs.
#[must_use]
pub fn expand_tabs(text: &str, tab_stop: usize) -> Cow<'_, str> {
    if !text.contains('\t') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut column = 0;
    for (_, grapheme, columns) in graphemes(text) {
        match grapheme {
            "\t" => {
                let spaces = if tab_stop == 0 {
                    0
                } else {
                    tab_stop - column % tab_stop
                };
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            "\n" | "\r\n" => {
                out.push_str(grapheme);
                column = 0;
            }
            _ => {
                out.push_str(grapheme);
                column += columns;
            }
        }
    }
    Cow::Owned(out)
}

/// Wraps `text` into lines of at most `max_width` columns.
///
/// See [`wrap_ranges`] for the rules. Each line is a slice of `text`.
#[must_use]
pub fn wrap(text: &str, max_width: usize) -> Vec<&str> {
    wrap_ranges(text, max_width)
        .into_iter()
        .map(|range| &text[range])
        .collect()
}

/// Wraps `text` into lines of at most `max_width` columns, and returns each
/// line as a byte range of `text`, so that a caller with styled text can
/// carry its styles across.
///
/// - A line break in the text (`\n` or `\r\n`) always ends a line. An empty
///   text is one empty line.
/// - Lines break at white space, which is dropped at the break. White space
///   at the start of a line of the original text (indentation) is kept. A
///   non-breaking space is not a place to break.
/// - A line may also break before or after a wide character, so that text
///   in scripts written without spaces wraps. A non-breaking space beside
///   the wide character holds the two together.
/// - A word wider than a whole line is broken between grapheme clusters.
/// - No line is wider than `max_width`, except where a single grapheme
///   cluster is: it then has a line to itself. A `max_width` of 0 is
///   treated as 1.
///
/// The ranges are in order and do not overlap, and what lies between them is
/// only the white space and line breaks that were dropped.
#[must_use]
pub fn wrap_ranges(text: &str, max_width: usize) -> Vec<Range<usize>> {
    let max_width = max_width.max(1);
    let mut lines = Vec::new();
    let mut paragraph_start = 0;
    for (newline, _) in text.match_indices('\n') {
        let end = if text[paragraph_start..newline].ends_with('\r') {
            newline - 1
        } else {
            newline
        };
        wrap_paragraph(text, paragraph_start..end, max_width, &mut lines);
        paragraph_start = newline + 1;
    }
    wrap_paragraph(text, paragraph_start..text.len(), max_width, &mut lines);
    lines
}

/// What a paragraph is made of, for wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece {
    /// Text that stays together.
    Word,
    /// White space: a place to break, dropped when a line breaks there.
    Space,
}

/// The spaces that hold what is on either side of them together: the
/// no-break space, the figure space and the narrow no-break space.
const NON_BREAKING_SPACES: [char; 3] = ['\u{a0}', '\u{2007}', '\u{202f}'];

/// Whether a grapheme is white space that a line may break at.
fn is_breaking_space(grapheme: &str) -> bool {
    grapheme
        .chars()
        .all(|character| character.is_whitespace() && !NON_BREAKING_SPACES.contains(&character))
}

/// Whether a grapheme is a space that forbids a break on either side of it.
fn is_glue(grapheme: &str) -> bool {
    grapheme.starts_with(NON_BREAKING_SPACES)
}

/// Wraps one paragraph (a stretch of `text` with no line break in it) and
/// appends its lines.
fn wrap_paragraph(
    text: &str,
    paragraph: Range<usize>,
    max_width: usize,
    lines: &mut Vec<Range<usize>>,
) {
    // The line being filled: where it starts, where its content ends so
    // far, and how wide that content is.
    let mut line_start = paragraph.start;
    let mut line_end = paragraph.start;
    let mut line_width = 0;
    // White space seen since the line's last word, not yet part of the line.
    let mut gap_width = 0;
    let mut line_has_word = false;

    let pieces = Pieces::new(&text[paragraph.clone()], paragraph.start);
    for (piece, range, piece_width) in pieces {
        match piece {
            Piece::Space => {
                if line_has_word {
                    gap_width += piece_width;
                } else {
                    // Indentation: white space before the paragraph's first
                    // word is content.
                    place_long(
                        text,
                        range,
                        max_width,
                        lines,
                        &mut line_start,
                        &mut line_end,
                        &mut line_width,
                    );
                }
            }
            Piece::Word => {
                if line_has_word && line_width + gap_width + piece_width <= max_width {
                    line_end = range.end;
                    line_width += gap_width + piece_width;
                } else {
                    if line_has_word {
                        lines.push(line_start..line_end);
                        line_start = range.start;
                        line_end = range.start;
                        line_width = 0;
                    }
                    place_long(
                        text,
                        range,
                        max_width,
                        lines,
                        &mut line_start,
                        &mut line_end,
                        &mut line_width,
                    );
                    line_has_word = true;
                }
                gap_width = 0;
            }
        }
    }
    // White space at the very end of the paragraph is kept if it fits.
    if gap_width > 0 && line_width + gap_width <= max_width {
        line_end = paragraph.end;
    }
    lines.push(line_start..line_end);
}

/// Adds a piece to the line being filled, breaking the piece between
/// grapheme clusters wherever the line is full. Used for a word that starts
/// a line, which may be wider than a whole line, and for indentation.
fn place_long(
    text: &str,
    piece: Range<usize>,
    max_width: usize,
    lines: &mut Vec<Range<usize>>,
    line_start: &mut usize,
    line_end: &mut usize,
    line_width: &mut usize,
) {
    for (offset, grapheme, columns) in graphemes(&text[piece.clone()]) {
        let start = piece.start + offset;
        if *line_width + columns > max_width && *line_end > *line_start {
            lines.push(*line_start..*line_end);
            *line_start = start;
            *line_width = 0;
        }
        *line_end = start + grapheme.len();
        *line_width += columns;
    }
}

/// Splits a paragraph into words and stretches of white space.
struct Pieces<'a> {
    graphemes: std::iter::Peekable<unicode_segmentation::GraphemeIndices<'a>>,
    /// The paragraph's offset in the whole text.
    base: usize,
}

impl<'a> Pieces<'a> {
    fn new(paragraph: &'a str, base: usize) -> Self {
        Self {
            graphemes: paragraph.grapheme_indices(true).peekable(),
            base,
        }
    }
}

impl Iterator for Pieces<'_> {
    type Item = (Piece, Range<usize>, usize);

    fn next(&mut self) -> Option<Self::Item> {
        let (start, first) = self.graphemes.next()?;
        let mut end = start + first.len();
        let mut total = grapheme_width(first);
        let piece = if is_breaking_space(first) {
            Piece::Space
        } else {
            Piece::Word
        };
        let mut last_is_wide = total >= 2;
        let mut last_is_glue = is_glue(first);
        while let Some(&(_, next)) = self.graphemes.peek() {
            let next_is_space = is_breaking_space(next);
            let next_width = grapheme_width(next);
            let joins = match piece {
                Piece::Space => next_is_space,
                Piece::Word if next_is_space => false,
                // A line can break on either side of a wide character,
                // unless a non-breaking space is what stands beside it.
                Piece::Word if last_is_wide || next_width >= 2 => last_is_glue || is_glue(next),
                Piece::Word => true,
            };
            if !joins {
                break;
            }
            end += next.len();
            total += next_width;
            last_is_wide = next_width >= 2;
            last_is_glue = is_glue(next);
            self.graphemes.next();
        }
        Some((piece, self.base + start..self.base + end, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::{self, ensure};
    use crate::rng::{Rng, SeededRng};

    const FAMILY: &str = "👨\u{200d}👩\u{200d}👧";
    const FLAG: &str = "🇯🇵";
    const E_ACUTE: &str = "e\u{301}";

    #[test]
    fn widths_follow_what_a_terminal_shows() {
        let cases = [
            ("", 0),
            ("abc", 3),
            ("naïve", 5),
            (E_ACUTE, 1),
            ("日本語", 6),
            ("ｱｲｳ", 3),
            ("🙂", 2),
            (FAMILY, 2),
            (FLAG, 2),
            ("👍🏽", 2),
            ("한글", 4),
            ("\u{1100}\u{1161}", 2),
            ("\u{200b}", 0),
            ("a\u{200b}b", 2),
            ("§→", 2),
            ("\t", 1),
            ("\u{1b}[31m", 5),
            ("\u{7f}", 1),
            ("\u{85}", 1),
            ("a\r\nb", 4),
            ("\u{301}", 0),
            ("x\u{a0}y", 3),
        ];
        for (text, expected) in cases {
            assert_eq!(width(text), expected, "{text:?}");
            let summed: usize = graphemes(text).map(|(_, _, columns)| columns).sum();
            assert_eq!(summed, expected, "{text:?} by graphemes");
        }
        assert_eq!(grapheme_width(""), 0);
        assert_eq!(width(ELLIPSIS), 1);
    }

    #[test]
    fn graphemes_report_where_they_start() {
        let text = format!("a{E_ACUTE}日{FAMILY}");
        let found: Vec<_> = graphemes(&text).collect();
        assert_eq!(
            found,
            [(0, "a", 1), (1, E_ACUTE, 1), (4, "日", 2), (7, FAMILY, 2),]
        );
    }

    #[test]
    fn truncation_keeps_the_start_by_default() {
        assert_eq!(truncate("hello", 5), "hello");
        assert!(matches!(truncate("hello", 5), Cow::Borrowed(_)));
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate("hello world", 1), "…");
        assert_eq!(truncate("hello world", 0), "");
        assert_eq!(truncate("", 0), "");
        // A wide character that does not fit leaves a column unused.
        assert_eq!(truncate("日本語日本語", 6), "日本…");
        assert_eq!(width(&truncate("日本語日本語", 6)), 5);
        assert_eq!(truncate("日本語日本語", 7), "日本語…");
        // A cluster is never split.
        assert_eq!(truncate(&format!("ab{FAMILY}cd"), 4), "ab…");
        assert_eq!(
            truncate(&format!("ab{FAMILY}cd"), 5),
            format!("ab{FAMILY}…")
        );
        assert_eq!(
            truncate(&format!("{E_ACUTE}{E_ACUTE}{E_ACUTE}"), 2),
            format!("{E_ACUTE}…")
        );
    }

    #[test]
    fn truncation_can_keep_the_end_or_both_ends() {
        assert_eq!(
            truncate_with("hello world", 8, ELLIPSIS, Keep::End),
            "…o world"
        );
        assert_eq!(
            truncate_with("hello world", 8, ELLIPSIS, Keep::Ends),
            "hell…rld"
        );
        assert_eq!(
            truncate_with("hello world", 7, ELLIPSIS, Keep::Ends),
            "hel…rld"
        );
        assert_eq!(
            truncate_with("src/components/editor/buffer.rs", 20, "...", Keep::Ends),
            "src/compo...uffer.rs"
        );
        assert_eq!(
            truncate_with("hello world", 8, " [cut]", Keep::Start),
            "he [cut]"
        );
        // No room for the marker: as much of the marker as fits.
        assert_eq!(truncate_with("hello world", 2, "...", Keep::Start), "..");
        assert_eq!(truncate_with("hello world", 3, "...", Keep::End), "...");
        assert_eq!(truncate_with("hello world", 1, "日", Keep::Start), "");
        // An empty marker is a plain cut.
        assert_eq!(truncate_with("hello world", 4, "", Keep::Start), "hell");
        assert_eq!(truncate_with("hello world", 4, "", Keep::End), "orld");
        assert_eq!(truncate_with("日本語", 5, "", Keep::Ends), "日語");
    }

    #[test]
    fn byte_truncation_ends_between_clusters() {
        assert_eq!(truncate_bytes("hello", 10), "hello");
        assert_eq!(truncate_bytes("hello", 3), "hel");
        assert_eq!(truncate_bytes("日本語", 4), "日");
        assert_eq!(truncate_bytes("日本語", 2), "");
        assert_eq!(truncate_bytes(&format!("a{FAMILY}b"), 10), "a");
        assert_eq!(
            truncate_bytes(&format!("a{FAMILY}b"), 19),
            format!("a{FAMILY}")
        );
        assert_eq!(truncate_bytes(E_ACUTE, 2), "");
    }

    #[test]
    fn tabs_expand_to_the_next_stop_on_each_line() {
        assert!(matches!(expand_tabs("no tabs", 8), Cow::Borrowed(_)));
        assert_eq!(expand_tabs("\tx", 4), "    x");
        assert_eq!(expand_tabs("ab\tx", 4), "ab  x");
        assert_eq!(expand_tabs("abcd\tx", 4), "abcd    x");
        assert_eq!(expand_tabs("日\tx", 4), "日  x");
        assert_eq!(expand_tabs("a\tb\n\tc\r\n\td", 4), "a   b\n    c\r\n    d");
        assert_eq!(expand_tabs("a\tb", 0), "ab");
        assert_eq!(expand_tabs("a\tb", 1), "a b");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_drops_them() {
        assert_eq!(wrap("the quick brown fox", 10), ["the quick", "brown fox"]);
        assert_eq!(wrap("the quick brown fox", 9), ["the quick", "brown fox"]);
        assert_eq!(
            wrap("the quick brown fox", 8),
            ["the", "quick", "brown", "fox"]
        );
        assert_eq!(wrap("the   quick", 6), ["the", "quick"]);
        assert_eq!(wrap("the   quick", 20), ["the   quick"]);
        assert_eq!(wrap("fits", 4), ["fits"]);
        assert_eq!(wrap("", 10), [""]);
    }

    #[test]
    fn wrapping_keeps_line_breaks_and_indentation() {
        assert_eq!(wrap("a\nb", 10), ["a", "b"]);
        assert_eq!(wrap("a\r\nb\r\n", 10), ["a", "b", ""]);
        assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"]);
        assert_eq!(
            wrap("  indented text here", 12),
            ["  indented", "text here"]
        );
        assert_eq!(wrap("    x", 3), ["   ", " x"]);
        assert_eq!(wrap("x  ", 3), ["x  "]);
        assert_eq!(wrap("xyz  ", 3), ["xyz"]);
        assert_eq!(wrap("   ", 2), ["  ", " "]);
        assert_eq!(
            wrap("a\rb", 10),
            ["a\rb"],
            "a lone carriage return is not a line break"
        );
    }

    #[test]
    fn a_word_wider_than_the_line_is_broken_between_clusters() {
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab cdefghij k", 4), ["ab", "cdef", "ghij", "k"]);
        let accents = E_ACUTE.repeat(5);
        assert_eq!(
            wrap(&accents, 2),
            [E_ACUTE.repeat(2), E_ACUTE.repeat(2), E_ACUTE.to_owned()]
        );
    }

    #[test]
    fn text_without_spaces_wraps_between_wide_characters() {
        assert_eq!(wrap("日本語のテキスト", 6), ["日本語", "のテキ", "スト"]);
        assert_eq!(
            wrap("日本語のテキスト", 5),
            ["日本", "語の", "テキ", "スト"]
        );
        assert_eq!(wrap("go 日本語 now", 7), ["go 日本", "語 now"]);
        assert_eq!(
            wrap(&format!("{FAMILY}{FAMILY}{FAMILY}"), 4),
            [FAMILY.repeat(2), FAMILY.to_owned()]
        );
        // A cluster wider than the line still gets a line.
        assert_eq!(wrap("a日b", 1), ["a", "日", "b"]);
        assert_eq!(wrap("ab", 0), ["a", "b"], "zero is treated as one");
    }

    #[test]
    fn a_non_breaking_space_holds_its_words_together() {
        assert_eq!(wrap("10\u{a0}km away", 6), ["10\u{a0}km", "away"]);
        assert_eq!(wrap("a\u{a0}b c", 3), ["a\u{a0}b", "c"]);
        // Also beside a wide character, where a break would otherwise be
        // allowed.
        assert_eq!(wrap("ab 日\u{a0}本 cd", 6), ["ab", "日\u{a0}本", "cd"]);
        assert_eq!(wrap("abc\u{a0}日 d", 6), ["abc\u{a0}日", "d"]);
        assert_eq!(wrap("日本\u{a0}c", 4), ["日", "本\u{a0}c"]);
    }

    #[test]
    fn wrap_ranges_point_into_the_text() {
        let text = "one two\nthree";
        assert_eq!(wrap_ranges(text, 4), [0..3, 4..7, 8..12, 12..13]);
        assert_eq!(wrap_ranges("", 4).as_slice(), std::slice::from_ref(&(0..0)));
    }

    /// Clusters that make text hard: wide, zero-width, combining, joined,
    /// control, and several kinds of space.
    const CLUSTERS: &[&str] = &[
        "a", "b", "z", "W", "0", ".", "-", "/", " ", " ", " ", "\t", "\n", "\r\n", "é", E_ACUTE,
        "日", "本", "語", "ｱ", "한", "🙂", FAMILY, FLAG, "👍🏽", "\u{200b}", "\u{a0}", "\u{1b}",
        "\u{3000}", "§", "ß", "\u{301}",
    ];

    fn any_text(rng: &mut SeededRng) -> String {
        let length = cases::small(rng, 60);
        (0..length).map(|_| *cases::pick(rng, CLUSTERS)).collect()
    }

    /// Text with no line break, for the properties that speak of one line.
    fn any_line(rng: &mut SeededRng) -> String {
        any_text(rng).replace(['\n', '\r'], " ")
    }

    #[test]
    fn property_truncation_never_exceeds_the_width_or_splits_a_cluster() {
        cases::check(
            "truncate",
            1500,
            |rng| {
                let keep = *cases::pick(rng, &[Keep::Start, Keep::End, Keep::Ends]);
                let marker = *cases::pick(rng, &[ELLIPSIS, "...", "", "日", " [cut]"]);
                (any_line(rng), rng.below(30) as usize, marker, keep)
            },
            |(text, max, marker, keep)| {
                let out = truncate_with(text, *max, marker, *keep);
                ensure!(width(&out) <= *max, "{out:?} is wider than {max}");
                if width(text) <= *max {
                    ensure!(out == *text, "text that fits was changed to {out:?}");
                    return Ok(());
                }
                ensure!(out != *text, "text that does not fit was returned whole");
                if width(marker) >= *max {
                    ensure!(
                        marker.starts_with(&*out),
                        "{out:?} is not the marker's start"
                    );
                    return Ok(());
                }
                // The result is a start of the text, the marker, and an end
                // of the text, each cut between clusters.
                let boundaries: Vec<usize> = text
                    .grapheme_indices(true)
                    .map(|(offset, _)| offset)
                    .chain([text.len()])
                    .collect();
                let split = boundaries.iter().find_map(|&head| {
                    let rest = out.strip_prefix(&text[..head])?.strip_prefix(marker)?;
                    let tail_start = text.len().checked_sub(rest.len())?;
                    let right_ends = match keep {
                        Keep::Start => tail_start == text.len(),
                        Keep::End => head == 0,
                        Keep::Ends => true,
                    };
                    (right_ends
                        && tail_start >= head
                        && boundaries.contains(&tail_start)
                        && &text[tail_start..] == rest)
                        .then_some(head)
                });
                let Some(head) = split else {
                    return Err(format!(
                        "{out:?} is not a start of the text, the marker and an end of the text"
                    )
                    .into());
                };
                // Nothing more would have fitted.
                if *keep == Keep::Start {
                    let next = text[head..].graphemes(true).next().unwrap_or("");
                    ensure!(
                        width(&out) + grapheme_width(next) > *max,
                        "the cluster {next:?} would still have fitted"
                    );
                }
                Ok(())
            },
        );
    }

    #[test]
    fn property_truncating_twice_changes_nothing_and_more_room_keeps_more() {
        cases::check(
            "truncate_stable",
            1500,
            |rng| (any_line(rng), rng.below(30) as usize),
            |(text, max)| {
                let once = truncate(text, *max);
                let twice = truncate(&once, *max);
                ensure!(once == twice, "{once:?} became {twice:?}");
                let wider = truncate(text, max + 1);
                ensure!(
                    width(&wider) >= width(&once),
                    "more room gave less: {wider:?} after {once:?}"
                );
                let kept = once.strip_suffix(ELLIPSIS).unwrap_or(&once);
                ensure!(
                    text.starts_with(kept),
                    "{kept:?} is not a start of the text"
                );
                Ok(())
            },
        );
    }

    #[test]
    fn property_byte_truncation_is_the_longest_whole_cluster_prefix() {
        cases::check(
            "truncate_bytes",
            1500,
            |rng| (any_text(rng), rng.below(40) as usize),
            |(text, max)| {
                let out = truncate_bytes(text, *max);
                ensure!(out.len() <= *max, "{out:?} is longer than {max} bytes");
                ensure!(text.starts_with(out), "not a start of the text");
                let ends: Vec<usize> = text
                    .grapheme_indices(true)
                    .map(|(offset, grapheme)| offset + grapheme.len())
                    .collect();
                ensure!(
                    out.is_empty() || ends.contains(&out.len()),
                    "a cluster was split"
                );
                let best = ends
                    .iter()
                    .copied()
                    .filter(|end| end <= max)
                    .max()
                    .unwrap_or(0);
                ensure!(
                    out.len() == best,
                    "kept {} bytes where {best} fit",
                    out.len()
                );
                Ok(())
            },
        );
    }

    #[test]
    fn property_wrapped_lines_fit_and_lose_only_white_space() {
        cases::check(
            "wrap",
            1500,
            |rng| (any_text(rng), 1 + rng.below(24) as usize),
            |(text, max)| {
                let ranges = wrap_ranges(text, *max);
                ensure!(!ranges.is_empty(), "no lines at all");
                let mut previous_end = 0;
                for range in &ranges {
                    ensure!(
                        range.start >= previous_end,
                        "lines overlap or go back: {ranges:?}"
                    );
                    ensure!(
                        range.end >= range.start && range.end <= text.len(),
                        "bad range"
                    );
                    ensure!(
                        text.is_char_boundary(range.start) && text.is_char_boundary(range.end),
                        "a line starts or ends inside a character"
                    );
                    let dropped = &text[previous_end..range.start];
                    ensure!(
                        dropped.chars().all(char::is_whitespace),
                        "dropped {dropped:?}, which is not white space"
                    );
                    let line = &text[range.clone()];
                    ensure!(!line.contains('\n'), "a line break inside {line:?}");
                    let clusters: Vec<_> = graphemes(line).collect();
                    ensure!(
                        width(line) <= *max || clusters.len() == 1,
                        "{line:?} is wider than {max}"
                    );
                    // The line is made of whole clusters of the text.
                    let whole: Vec<usize> = text
                        .grapheme_indices(true)
                        .map(|(offset, _)| offset)
                        .chain([text.len()])
                        .collect();
                    ensure!(
                        whole.contains(&range.start) && whole.contains(&range.end),
                        "{line:?} splits a cluster"
                    );
                    previous_end = range.end;
                }
                let dropped = &text[previous_end..];
                ensure!(
                    dropped.chars().all(char::is_whitespace),
                    "the end {dropped:?} was lost"
                );
                ensure!(
                    ranges.len() > text.matches('\n').count(),
                    "fewer lines than line breaks"
                );
                Ok(())
            },
        );
    }

    #[test]
    fn property_wrapping_is_greedy_and_stable() {
        cases::check(
            "wrap_greedy",
            1500,
            |rng| (any_line(rng), 1 + rng.below(24) as usize),
            |(text, max)| {
                let ranges = wrap_ranges(text, *max);
                let lines: Vec<&str> = ranges.iter().map(|range| &text[range.clone()]).collect();
                if width(text) <= *max {
                    ensure!(
                        lines == [text.as_str()],
                        "text that fits was wrapped: {lines:?}"
                    );
                }
                for pair in ranges.windows(2) {
                    let (line, next) = (&text[pair[0].clone()], &text[pair[1].clone()]);
                    let gap = width(&text[pair[0].end..pair[1].start]);
                    // The word the next line starts with did not fit on this
                    // one. The whole word, as it stands in the text: a word
                    // wider than a line starts on a line of its own, and is
                    // not used to fill up the line before it.
                    let first_word = Pieces::new(&text[pair[1].start..], 0)
                        .next()
                        .filter(|(piece, _, _)| *piece == Piece::Word)
                        .map_or(0, |(_, _, columns)| columns);
                    ensure!(
                        first_word == 0 || width(line) + gap + first_word > *max,
                        "the first word of {next:?} would have fitted after {line:?}"
                    );
                    // Only indentation puts a breaking space at the start of
                    // a line.
                    let indentation =
                        graphemes(line).all(|(_, grapheme, _)| is_breaking_space(grapheme));
                    let starts_with_space = graphemes(next)
                        .next()
                        .is_some_and(|(_, grapheme, _)| is_breaking_space(grapheme));
                    ensure!(
                        indentation || !starts_with_space,
                        "a wrapped line starts with white space: {next:?}"
                    );
                }
                // Wrapping each line again at the same width changes nothing.
                for line in &lines {
                    let again = wrap(line, *max);
                    ensure!(again == [*line], "{line:?} wrapped again into {again:?}");
                }
                Ok(())
            },
        );
    }

    #[test]
    fn property_width_is_the_sum_of_its_parts() {
        cases::check(
            "width_sum",
            1500,
            |rng| (any_text(rng), any_text(rng)),
            |(first, second)| {
                let by_clusters: usize = graphemes(first).map(|(_, _, columns)| columns).sum();
                ensure!(
                    width(first) == by_clusters,
                    "width disagrees with its clusters"
                );
                // A line break between two texts keeps their clusters apart,
                // so the widths add up.
                let joined = format!("{first}\n{second}");
                ensure!(
                    width(&joined) == width(first) + 1 + width(second),
                    "{joined:?} is not as wide as its parts"
                );
                Ok(())
            },
        );
    }
}
