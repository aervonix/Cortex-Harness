//! Text from models, tools and files made safe to show
//! (`docs/architecture/09-tui.md`, "Sanitising").
//!
//! Only the renderer emits control sequences. Every control character in
//! content is shown as a visible symbol instead, so content can never move
//! the cursor, change a mode, set a title, write a link, or ask the terminal
//! anything. Two layers make sure of it:
//!
//! - [`sanitize`], the one function content passes through before a
//!   component lays it out. It keeps line breaks and tabs, which layout
//!   needs, and makes every other control character visible.
//! - [`visible`], which the renderer applies to every character it writes
//!   from a [`Line`](crate::Line), line breaks and tabs included. A component
//!   that forgot the first layer still cannot send an escape sequence.
//!
//! The control characters are C0 (`U+0000` to `U+001F`), DEL (`U+007F`) and
//! C1 (`U+0080` to `U+009F`). C1 matters as much as ESC: some terminals take
//! `U+009B` as the start of a control sequence, exactly like `ESC [`.

use std::borrow::Cow;

/// What a C1 control character is shown as. They have no picture of their
/// own in Unicode.
const C1_SYMBOL: char = '\u{FFFD}';

/// The visible stand-in for a control character, or `None` for a character
/// that is not one.
///
/// C0 characters become their Control Pictures (`␛` for ESC, `␇` for BEL),
/// DEL becomes `␡`, and C1 characters become `�`. Each stand-in is one column
/// wide, as the control character is counted
/// ([`cortex_base::text::width`]).
#[must_use]
pub fn visible(c: char) -> Option<char> {
    match c {
        '\0'..='\u{1f}' => char::from_u32(0x2400 + u32::from(c)),
        '\u{7f}' => Some('\u{2421}'),
        '\u{80}'..='\u{9f}' => Some(C1_SYMBOL),
        _ => None,
    }
}

/// `text` with every control character but the line feed and the tab made
/// visible. A carriage return just before a line feed is dropped, so text
/// with Windows line endings is plain lines.
///
/// Text with nothing to change is returned as it is, borrowed.
#[must_use]
pub fn sanitize(text: &str) -> Cow<'_, str> {
    let needs = text
        .chars()
        .any(|c| c != '\n' && c != '\t' && visible(c).is_some());
    if !needs {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\t' => out.push(c),
            '\r' if chars.peek() == Some(&'\n') => {}
            _ => out.push(visible(c).unwrap_or(c)),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_character_but_line_feed_and_tab_is_made_visible() {
        assert_eq!(sanitize("plain text\n\tindented"), "plain text\n\tindented");
        assert!(matches!(sanitize("plain"), Cow::Borrowed(_)));
        assert_eq!(sanitize("a\u{1b}[2Jb"), "a\u{241b}[2Jb");
        assert_eq!(sanitize("bell\u{7}"), "bell\u{2407}");
        assert_eq!(sanitize("del\u{7f}"), "del\u{2421}");
        assert_eq!(sanitize("csi\u{9b}31m"), "csi\u{fffd}31m");
        assert_eq!(sanitize("win\r\nline"), "win\nline");
        assert_eq!(sanitize("bare\rreturn"), "bare\u{240d}return");
        // Nothing that is left could start a sequence.
        for c in (0..=0x10ffff).filter_map(char::from_u32) {
            let shown = sanitize(&c.to_string()).into_owned();
            assert!(
                shown
                    .chars()
                    .all(|s| s == '\n' || s == '\t' || visible(s).is_none()),
                "{c:?}"
            );
        }
    }

    #[test]
    fn every_stand_in_is_one_column() {
        for c in (0..=0x9f).filter_map(char::from_u32) {
            if let Some(shown) = visible(c) {
                assert_eq!(cortex_base::text::width(&shown.to_string()), 1, "{c:?}");
                assert_eq!(cortex_base::text::width(&c.to_string()), 1, "{c:?}");
            }
        }
    }
}
