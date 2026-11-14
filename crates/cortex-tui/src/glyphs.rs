//! The two glyph sets: Unicode and ASCII (`docs/architecture/09-tui.md`,
//! "Colour and glyphs").
//!
//! `tui.glyphs = "auto"` uses the Unicode set when the terminal shows Unicode
//! (`Capabilities::unicode`: a UTF-8 locale, and not the old console host),
//! and the ASCII set otherwise. A view never writes a glyph of its own: it
//! takes it from the set it is given, so that one switch changes every one.
//!
//! Every glyph of the ASCII set is printable ASCII. Every glyph of either
//! set is one column wide, except the ASCII ellipsis, arrow and plan marks,
//! which are words of two or three columns. A view measures what it draws
//! and never assumes a glyph's width.

use cortex_term::Capabilities;

/// One glyph set. See the [module documentation](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    /// Your message and the input box: `◆`.
    pub user: &'static str,
    /// The answer: `◇`.
    pub answer: &'static str,
    /// Reasoning: `∴`.
    pub reasoning: &'static str,
    /// A call that succeeded: `✓`.
    pub succeeded: &'static str,
    /// A call that failed, and an error: `✗`.
    pub failed: &'static str,
    /// A call waiting for approval, and a dialog's question: `?`.
    pub waiting: &'static str,
    /// A call that was denied: `⊘`.
    pub denied: &'static str,
    /// A call that was interrupted: `■`.
    pub interrupted: &'static str,
    /// The spinner's frames, shown one after another every 100 ms.
    pub spinner: &'static [&'static str],
    /// A notice: `·`.
    pub notice: &'static str,
    /// What separates the parts of a measure or a hint: `·`, read as "and".
    pub separator: &'static str,
    /// The corners and edges of a rounded box.
    pub corner_top_left: &'static str,
    /// The top right corner: `╮`.
    pub corner_top_right: &'static str,
    /// The bottom left corner: `╰`.
    pub corner_bottom_left: &'static str,
    /// The bottom right corner: `╯`.
    pub corner_bottom_right: &'static str,
    /// A box's top and bottom edge, and a rule: `─`.
    pub horizontal: &'static str,
    /// A box's side, and a quote's bar: `│`.
    pub vertical: &'static str,
    /// A plan item still to do: `☐`.
    pub plan_todo: &'static str,
    /// The active plan item: `◧`.
    pub plan_active: &'static str,
    /// A plan item that is done: `☑`.
    pub plan_done: &'static str,
    /// A plan item that was skipped: `⊘`.
    pub plan_skipped: &'static str,
    /// An unordered list item: `–`.
    pub bullet: &'static str,
    /// A removed line's sign, and the count of removed lines: `−`.
    pub minus: &'static str,
    /// Where text was cut: `…`.
    pub ellipsis: &'static str,
    /// From one value to another: `→`.
    pub arrow: &'static str,
    /// A range of numbers, as in `lines 120–178`: `–`.
    pub range: &'static str,
}

/// The spinner of the Unicode set: an eight-frame braille cycle.
const BRAILLE: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// The spinner of the ASCII set.
const BAR: [&str; 4] = ["|", "/", "-", "\\"];

impl Glyphs {
    /// The Unicode set, for a terminal that shows it.
    pub const UNICODE: Self = Self {
        user: "◆",
        answer: "◇",
        reasoning: "∴",
        succeeded: "✓",
        failed: "✗",
        waiting: "?",
        denied: "⊘",
        interrupted: "■",
        spinner: &BRAILLE,
        notice: "·",
        separator: "·",
        corner_top_left: "╭",
        corner_top_right: "╮",
        corner_bottom_left: "╰",
        corner_bottom_right: "╯",
        horizontal: "─",
        vertical: "│",
        plan_todo: "☐",
        plan_active: "◧",
        plan_done: "☑",
        plan_skipped: "⊘",
        bullet: "–",
        minus: "−",
        ellipsis: "…",
        arrow: "→",
        range: "–",
    };

    /// The ASCII set, for a terminal without Unicode (and the old console
    /// host).
    pub const ASCII: Self = Self {
        user: ">",
        answer: "o",
        reasoning: "~",
        succeeded: "+",
        failed: "x",
        waiting: "?",
        denied: "-",
        interrupted: "#",
        spinner: &BAR,
        notice: ".",
        separator: "|",
        corner_top_left: "+",
        corner_top_right: "+",
        corner_bottom_left: "+",
        corner_bottom_right: "+",
        horizontal: "-",
        vertical: "|",
        plan_todo: "[ ]",
        plan_active: "[>]",
        plan_done: "[x]",
        plan_skipped: "[-]",
        bullet: "-",
        minus: "-",
        ellipsis: "...",
        arrow: "->",
        range: "-",
    };

    /// The set for a terminal: Unicode where it shows Unicode, ASCII
    /// otherwise. This is `tui.glyphs = "auto"`.
    #[must_use]
    pub fn for_terminal(caps: &Capabilities) -> &'static Self {
        if caps.unicode {
            &Self::UNICODE
        } else {
            &Self::ASCII
        }
    }

    /// The spinner's frame for a tick: the frames in turn, from the first.
    #[must_use]
    pub fn spinner_frame(&self, tick: usize) -> &'static str {
        self.spinner[tick % self.spinner.len()]
    }

    /// The mark shown in place of the spinner when `tui.reduced_motion` is
    /// on: its first frame, still.
    #[must_use]
    pub fn still_spinner(&self) -> &'static str {
        self.spinner[0]
    }

    /// Every glyph of the set, for checks that hold of all of them.
    #[must_use]
    pub fn all(&self) -> Vec<&'static str> {
        let mut all = vec![
            self.user,
            self.answer,
            self.reasoning,
            self.succeeded,
            self.failed,
            self.waiting,
            self.denied,
            self.interrupted,
            self.notice,
            self.separator,
            self.corner_top_left,
            self.corner_top_right,
            self.corner_bottom_left,
            self.corner_bottom_right,
            self.horizontal,
            self.vertical,
            self.plan_todo,
            self.plan_active,
            self.plan_done,
            self.plan_skipped,
            self.bullet,
            self.minus,
            self.ellipsis,
            self.arrow,
            self.range,
        ];
        all.extend_from_slice(self.spinner);
        all
    }
}

#[cfg(test)]
mod tests {
    use cortex_term::{Overrides, Platform};

    use super::*;

    #[test]
    fn the_ascii_set_is_printable_ascii_and_the_unicode_set_is_narrow() {
        for glyph in Glyphs::ASCII.all() {
            assert!(!glyph.is_empty());
            assert!(
                glyph.bytes().all(|byte| (0x20..0x7f).contains(&byte)),
                "{glyph:?} is not printable ASCII"
            );
        }
        // The layout never depends on a glyph being narrow where a terminal
        // may draw it wide; each Unicode glyph is one column as counted.
        for glyph in Glyphs::UNICODE.all() {
            assert_eq!(cortex_base::text::width(glyph), 1, "{glyph:?}");
        }
        // The box's edges are one column in both sets, so a box has the
        // same shape in each.
        for set in [Glyphs::UNICODE, Glyphs::ASCII] {
            for edge in [
                set.corner_top_left,
                set.corner_top_right,
                set.corner_bottom_left,
                set.corner_bottom_right,
                set.horizontal,
                set.vertical,
            ] {
                assert_eq!(cortex_base::text::width(edge), 1, "{edge:?}");
            }
        }
    }

    #[test]
    fn the_terminal_chooses_the_set_and_the_spinner_cycles() {
        let caps = |unicode| {
            Capabilities::detect(
                |_| None,
                Platform::Unix,
                Overrides {
                    unicode: Some(unicode),
                    ..Overrides::default()
                },
            )
        };
        assert_eq!(Glyphs::for_terminal(&caps(true)), &Glyphs::UNICODE);
        assert_eq!(Glyphs::for_terminal(&caps(false)), &Glyphs::ASCII);
        assert_eq!(Glyphs::UNICODE.spinner.len(), 8);
        assert_eq!(Glyphs::UNICODE.spinner_frame(0), "⣾");
        assert_eq!(Glyphs::UNICODE.spinner_frame(9), "⣽");
        assert_eq!(Glyphs::ASCII.spinner_frame(5), "/");
        assert_eq!(Glyphs::ASCII.still_spinner(), "|");
    }
}
