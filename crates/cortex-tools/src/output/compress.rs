//! Structural compression: what is taken out of produced output before the
//! size limits apply (`docs/architecture/15-efficiency.md`, "Compressed tool
//! output", stage 1).
//!
//! - Terminal control sequences and colour codes are removed.
//! - A line that was redrawn with carriage returns collapses to its final
//!   state.
//! - A run of identical lines collapses to the line and a count.
//! - A run of lines that differ only in their numbers collapses to its first
//!   and last line and a count. This one loses text, so it is done only to
//!   output that is over the limits and would be cut anyway.
//!
//! Every function here is pure. None of them is applied to content that was
//! asked for as it is: a file read is returned exactly.

use std::borrow::Cow;

use super::count;

/// A run of identical lines is collapsed when it is at least this long, so
/// that collapsing always saves lines.
const MIN_REPEATS: usize = 4;

/// A run of similar lines is collapsed when it is at least this long.
const MIN_SIMILAR: usize = 4;

/// How many letters a line's shape needs before lines of that shape count as
/// similar. Rows of numbers have none, and are data, not progress.
const MIN_SHAPE_LETTERS: usize = 3;

/// Removes terminal control sequences and control characters from `text`.
///
/// What stays: line feeds, tabs, and carriage returns (which
/// [`final_state`] reads). What goes: escape sequences (colours, cursor
/// movement, titles, hyperlinks), and every other control character.
pub(crate) fn strip_control(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_stripped) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\u{1b}' => skip_escape(&mut chars),
            // The one-character form of `ESC [`.
            '\u{9b}' => skip_control_sequence(&mut chars),
            character if is_stripped(character) => {}
            character => out.push(character),
        }
    }
    Cow::Owned(out)
}

fn is_stripped(character: char) -> bool {
    match character {
        '\n' | '\t' | '\r' => false,
        character => character.is_control(),
    }
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Skips what follows an escape character.
fn skip_escape(chars: &mut Chars<'_>) {
    match chars.peek().copied() {
        // A control sequence: `ESC [`, parameters, a final byte.
        Some('[') => {
            chars.next();
            skip_control_sequence(chars);
        }
        // A string: `ESC ]` (and its rarer relatives) up to a bell or the
        // string terminator `ESC \`.
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            skip_string(chars);
        }
        // An escape with intermediate bytes, such as a character-set
        // choice: `ESC ( B`.
        Some(' '..='/') => {
            while matches!(chars.peek(), Some(' '..='/')) {
                chars.next();
            }
            if matches!(chars.peek(), Some('0'..='~')) {
                chars.next();
            }
        }
        // A two-character escape.
        Some('0'..='~') => {
            chars.next();
        }
        // A lone escape: only the escape goes.
        _ => {}
    }
}

/// Skips the parameters, intermediates and final byte of a control
/// sequence.
fn skip_control_sequence(chars: &mut Chars<'_>) {
    while matches!(chars.peek(), Some('0'..='?')) {
        chars.next();
    }
    while matches!(chars.peek(), Some(' '..='/')) {
        chars.next();
    }
    if matches!(chars.peek(), Some('@'..='~')) {
        chars.next();
    }
}

/// Skips a control string up to its terminator. One that is never
/// terminated ends with its line, so it cannot swallow the output after it.
fn skip_string(chars: &mut Chars<'_>) {
    while let Some(character) = chars.peek().copied() {
        match character {
            '\n' => return,
            '\u{7}' | '\u{9c}' => {
                chars.next();
                return;
            }
            '\u{1b}' => {
                chars.next();
                if chars.peek() == Some(&'\\') {
                    chars.next();
                }
                return;
            }
            _ => {
                chars.next();
            }
        }
    }
}

/// One line as it stood when the output ended: what a terminal would show
/// after every redraw. A carriage return sends the cursor to the start of
/// the line, and a progress display then writes the line again, so the last
/// piece that has any text is the final state. The carriage return of a
/// CRLF line ending goes too.
pub(crate) fn final_state(line: &str) -> &str {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if !line.contains('\r') {
        return line;
    }
    line.rsplit('\r')
        .find(|piece| !piece.is_empty())
        .unwrap_or("")
}

/// Collapses runs of identical lines to the line and a count.
pub(crate) fn collapse_repeats<'a>(lines: Vec<Cow<'a, str>>) -> (Vec<Cow<'a, str>>, usize) {
    let mut out = Vec::with_capacity(lines.len());
    let mut collapsed = 0;
    let mut index = 0;
    while index < lines.len() {
        let mut end = index + 1;
        while end < lines.len() && lines[end] == lines[index] {
            end += 1;
        }
        let run = end - index;
        if run >= MIN_REPEATS {
            out.push(lines[index].clone());
            out.push(Cow::Owned(repeat_marker(run - 1)));
            collapsed += run - 1;
        } else {
            out.extend(lines[index..end].iter().cloned());
        }
        index = end;
    }
    (out, collapsed)
}

/// Collapses runs of lines that differ only in their numbers to the first
/// and the last of the run, with a count between them.
pub(crate) fn collapse_similar<'a>(lines: Vec<Cow<'a, str>>) -> (Vec<Cow<'a, str>>, usize) {
    let shapes: Vec<Option<String>> = lines.iter().map(|line| shape_of(line)).collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut collapsed = 0;
    let mut index = 0;
    while index < lines.len() {
        let mut end = index + 1;
        if shapes[index].is_some() {
            while end < lines.len() && shapes[end] == shapes[index] {
                end += 1;
            }
        }
        let run = end - index;
        if run >= MIN_SIMILAR {
            out.push(lines[index].clone());
            out.push(Cow::Owned(similar_marker(run - 2)));
            out.push(lines[end - 1].clone());
            collapsed += run - 2;
        } else {
            out.extend(lines[index..end].iter().cloned());
        }
        index = end;
    }
    (out, collapsed)
}

/// A line with each run of digits replaced by `#`, if what is left has
/// enough letters to be a message and not a row of data.
fn shape_of(line: &str) -> Option<String> {
    if is_marker(line) {
        return None;
    }
    let mut shape = String::with_capacity(line.len());
    let mut letters = 0;
    let mut in_number = false;
    for character in line.chars() {
        if character.is_ascii_digit() {
            if !in_number {
                shape.push('#');
            }
            in_number = true;
        } else {
            in_number = false;
            letters += usize::from(character.is_alphabetic());
            shape.push(character);
        }
    }
    (letters >= MIN_SHAPE_LETTERS).then_some(shape)
}

/// The line that stands for `more` further copies of the line above it.
pub(crate) fn repeat_marker(more: usize) -> String {
    let times = if more == 1 { "time" } else { "times" };
    format!("[the line above repeats {} more {times}]", count(more))
}

/// The line that stands for `hidden` similar lines.
pub(crate) fn similar_marker(hidden: usize) -> String {
    let lines = if hidden == 1 { "line" } else { "lines" };
    format!("[… {} similar {lines} not shown …]", count(hidden))
}

/// Whether a line is one of this module's own markers. A marker is never
/// folded into a run of similar lines.
fn is_marker(line: &str) -> bool {
    line.starts_with("[the line above repeats ") || line.starts_with("[… ")
}

/// Whether a line reports a problem: the lines that are kept ahead of
/// routine ones when the output of a build or a test run must be cut.
pub(crate) fn is_important(line: &str) -> bool {
    const SIGNS: [&str; 9] = [
        "error",
        "fail",
        "panic",
        "warning",
        "exception",
        "fatal",
        "traceback",
        "assert",
        "denied",
    ];
    let bytes = line.as_bytes();
    SIGNS.iter().any(|sign| {
        let sign = sign.as_bytes();
        bytes
            .windows(sign.len())
            .any(|window| window.eq_ignore_ascii_case(sign))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<Cow<'static, str>> {
        text.iter()
            .map(|line| Cow::Owned((*line).to_owned()))
            .collect()
    }

    fn texts(lines: &[Cow<'_, str>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn colour_codes_and_cursor_movement_are_removed() {
        assert_eq!(strip_control("\u{1b}[1;31merror\u{1b}[0m: no"), "error: no");
        assert_eq!(strip_control("a\u{1b}[2Kb\u{1b}[10;5Hc"), "abc");
        assert_eq!(strip_control("\u{9b}32mgreen\u{9b}m"), "green");
        // A private-mode sequence, and one with an intermediate byte.
        assert_eq!(strip_control("\u{1b}[?25lhidden\u{1b}[?25h"), "hidden");
        assert_eq!(strip_control("x\u{1b}[0 qy"), "xy");
    }

    #[test]
    fn titles_and_hyperlinks_are_removed_with_either_terminator() {
        assert_eq!(strip_control("\u{1b}]0;my title\u{7}text"), "text");
        assert_eq!(
            strip_control("\u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(strip_control("\u{1b}Pdevice string\u{1b}\\after"), "after");
    }

    #[test]
    fn an_unfinished_string_ends_with_its_line() {
        assert_eq!(
            strip_control("\u{1b}]0;never closed\nthe next line stays"),
            "\nthe next line stays"
        );
        assert_eq!(strip_control("tail \u{1b}"), "tail ");
        assert_eq!(strip_control("tail \u{1b}["), "tail ");
    }

    #[test]
    fn other_escapes_and_control_characters_are_removed() {
        assert_eq!(strip_control("a\u{1b}(Bb"), "ab");
        assert_eq!(strip_control("a\u{1b}7b\u{1b}8c"), "abc");
        assert_eq!(
            strip_control("bell\u{7} back\u{8}space nul\0 del\u{7f}"),
            "bell backspace nul del"
        );
        assert_eq!(strip_control("c1\u{85}gone"), "c1gone");
    }

    #[test]
    fn plain_text_is_returned_as_it_is() {
        let text = "tabs\tand\nlines\r\nstay, as does ünïcödé ✓";
        assert!(matches!(strip_control(text), Cow::Borrowed(_)));
        assert_eq!(strip_control(text), text);
    }

    #[test]
    fn a_redrawn_line_collapses_to_its_final_state() {
        assert_eq!(final_state("10%\r50%\r100%"), "100%");
        assert_eq!(final_state("Downloading\r\r"), "Downloading");
        assert_eq!(final_state("working...\rdone\r"), "done");
        assert_eq!(final_state("windows line\r"), "windows line");
        assert_eq!(final_state("plain"), "plain");
        assert_eq!(final_state("\r\r"), "");
        assert_eq!(final_state(""), "");
    }

    #[test]
    fn a_run_of_identical_lines_becomes_the_line_and_a_count() {
        let (out, collapsed) = collapse_repeats(lines(&["a", "b", "b", "b", "b", "b", "c"]));
        assert_eq!(
            texts(&out),
            ["a", "b", "[the line above repeats 4 more times]", "c"]
        );
        assert_eq!(collapsed, 4);
    }

    #[test]
    fn a_short_run_is_left_alone() {
        let input = lines(&["a", "a", "a", "b", "b", "", ""]);
        let (out, collapsed) = collapse_repeats(input.clone());
        assert_eq!(out, input);
        assert_eq!(collapsed, 0);
    }

    #[test]
    fn blank_lines_are_counted_like_any_other() {
        let (out, collapsed) = collapse_repeats(lines(&["a", "", "", "", "", "b"]));
        assert_eq!(
            texts(&out),
            ["a", "", "[the line above repeats 3 more times]", "b"]
        );
        assert_eq!(collapsed, 3);
    }

    #[test]
    fn lines_that_differ_only_in_numbers_collapse_to_first_and_last() {
        let (out, collapsed) = collapse_similar(lines(&[
            "start",
            "Downloading 1/5",
            "Downloading 2/5",
            "Downloading 3/5",
            "Downloading 4/5",
            "Downloading 5/5",
            "done",
        ]));
        assert_eq!(
            texts(&out),
            [
                "start",
                "Downloading 1/5",
                "[… 3 similar lines not shown …]",
                "Downloading 5/5",
                "done"
            ]
        );
        assert_eq!(collapsed, 3);
    }

    #[test]
    fn rows_of_numbers_are_data_and_are_not_collapsed() {
        let input = lines(&["1.5,2.5", "3.5,4.5", "5.5,6.5", "7.5,8.5", "9.5,0.5"]);
        let (out, collapsed) = collapse_similar(input.clone());
        assert_eq!(out, input);
        assert_eq!(collapsed, 0);
    }

    #[test]
    fn lines_with_different_words_are_not_similar() {
        let input = lines(&[
            "Compiling alpha v1.0.0",
            "Compiling beta v1.0.0",
            "Compiling gamma v1.0.0",
            "Compiling delta v1.0.0",
        ]);
        let (out, collapsed) = collapse_similar(input.clone());
        assert_eq!(out, input);
        assert_eq!(collapsed, 0);
    }

    #[test]
    fn a_marker_is_never_folded_into_a_run() {
        let marker = repeat_marker(12);
        let input = lines(&[&marker, &marker, &marker, &marker]);
        let (out, collapsed) = collapse_similar(input.clone());
        assert_eq!(out, input);
        assert_eq!(collapsed, 0);
        assert_eq!(repeat_marker(1), "[the line above repeats 1 more time]");
        assert_eq!(similar_marker(1), "[… 1 similar line not shown …]");
        assert_eq!(similar_marker(1500), "[… 1,500 similar lines not shown …]");
    }

    #[test]
    fn problem_lines_are_recognised_whatever_their_case() {
        for line in [
            "error[E0308]: mismatched types",
            "test result: FAILED. 1 passed; 2 failed",
            "thread 'main' panicked at src/main.rs:3",
            "Warning: unused variable",
            "java.lang.NullPointerException",
            "Traceback (most recent call last):",
            "AssertionError: 1 != 2",
            "Permission denied",
            "FATAL: could not connect",
        ] {
            assert!(is_important(line), "{line}");
        }
        for line in ["Compiling serde v1.0", "test parse ... ok", "", "42"] {
            assert!(!is_important(line), "{line}");
        }
    }
}
