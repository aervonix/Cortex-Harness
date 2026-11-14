use cortex_term::{ColorDepth, Span};

use super::*;
use crate::testing::{DARK, assert_snapshot, problems, sections, variants};
use crate::theme::{Theme, Token};

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(|line| line.text().into_owned()).collect()
}

#[test]
fn a_box_is_exactly_as_wide_as_it_is_told() {
    let theme = Theme::new(ColorDepth::TrueColor, Some(DARK));
    let border = theme.style(Token::Border);
    let label = Line::styled("ctrl+t expands", theme.style(Token::Hint));
    let rows = [
        (Line::plain("✓ Read lex.rs  lines 120–178"), Style::PLAIN),
        (Line::plain("漢字漢字漢字漢字漢字漢字"), Style::PLAIN),
    ];
    let lines = RoundedBox::new(24, &Glyphs::UNICODE, border).draw(&rows, Some(&label));
    assert_eq!(
        texts(&lines),
        [
            "╭──────────────────────╮",
            "│ ✓ Read lex.rs  line… │",
            "│ 漢字漢字漢字漢字漢…  │",
            "╰───── ctrl+t expands ─╯",
        ]
    );
    for line in &lines {
        assert_eq!(line.width(), 24, "{:?}", line.text());
    }
    // The border is in the border's style, the label in its own.
    let bottom = &lines[3];
    assert_eq!(bottom.spans()[0].style(), border);
    assert!(
        bottom
            .spans()
            .iter()
            .any(|span| span.text() == "ctrl+t expands" && span.style() == theme.style(Token::Hint))
    );
    // The fill reaches the space before the right border.
    let tint = theme.style(Token::DiffAddLine);
    let row = RoundedBox::new(12, &Glyphs::UNICODE, border).row(&Line::plain("ab"), tint);
    assert_eq!(row.text(), "│ ab       │");
    assert_eq!(row.spans()[3].style(), tint);
    assert_eq!(row.spans()[3].text(), "      ");
}

#[test]
fn a_label_too_long_is_cut_and_one_with_no_room_is_left_out() {
    let style = Style::PLAIN;
    let label = Line::plain("ctrl+t collapses");
    let ascii = RoundedBox::new(14, &Glyphs::ASCII, style);
    assert_eq!(ascii.bottom(Some(&label)).text(), "+ ctrl+t... -+");
    assert_eq!(ascii.bottom(Some(&label)).width(), 14);
    let unicode = RoundedBox::new(14, &Glyphs::UNICODE, style);
    assert_eq!(unicode.bottom(Some(&label)).text(), "╰ ctrl+t c… ─╯");
    assert_eq!(unicode.bottom(Some(&label)).width(), 14);
    assert_eq!(unicode.bottom(Some(&Line::new())).text(), "╰────────────╯");
    let tiny = RoundedBox::new(6, &Glyphs::UNICODE, style);
    assert_eq!(tiny.bottom(Some(&label)).text(), "╰────╯");
}

#[test]
fn no_row_of_any_box_passes_its_width() {
    let label = Line::plain("ctrl+o expands");
    let rows = [
        (
            Line::plain("👨\u{200d}👩\u{200d}👧 a family, and more text after it"),
            Style::PLAIN,
        ),
        (
            Line::from(Span::plain("e\u{301}e\u{301}e\u{301}e\u{301}")),
            Style::PLAIN,
        ),
        (Line::new(), Style::PLAIN),
    ];
    for glyphs in [&Glyphs::UNICODE, &Glyphs::ASCII] {
        for width in 0..40 {
            let lines = RoundedBox::new(width, glyphs, Style::PLAIN).draw(&rows, Some(&label));
            let found = problems(&lines, width);
            assert!(found.is_empty(), "{width}: {found:?}");
            if width >= 4 {
                assert!(lines.iter().all(|line| line.width() == width), "{width}");
                // Every row keeps both its borders: none was cut to fit.
                let last = lines.len() - 1;
                for (index, line) in lines.iter().enumerate() {
                    let text = line.text();
                    let (left, right) = match index {
                        0 => (glyphs.corner_top_left, glyphs.corner_top_right),
                        _ if index == last => {
                            (glyphs.corner_bottom_left, glyphs.corner_bottom_right)
                        }
                        _ => (glyphs.vertical, glyphs.vertical),
                    };
                    assert!(
                        text.starts_with(left) && text.ends_with(right),
                        "{width}: {text:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn snapshot() {
    let mut parts = Vec::new();
    for variant in variants() {
        let theme = &variant.theme;
        let rounded = RoundedBox::new(variant.width(), variant.glyphs, theme.style(Token::Border));
        let user = Line::from(vec![
            Span::new(variant.glyphs.user, theme.style(Token::User)),
            Span::plain(" "),
            Span::new(
                "Describe a task, or ask a question",
                theme.style(Token::Hint),
            ),
        ]);
        let label = Line::styled("ctrl+t collapses", theme.style(Token::Hint));
        let mut lines = rounded.draw(&[(user, Style::PLAIN)], None);
        let long = Line::plain(
            "test result: FAILED. 47 passed; 1 failed; 0 ignored; finished in 0.02s, and then some more",
        );
        lines.extend(rounded.draw(
            &[
                (long, Style::PLAIN),
                (Line::plain("+ added"), theme.style(Token::DiffAddLine)),
            ],
            Some(&label),
        ));
        parts.push((variant.title(), lines));
    }
    assert_snapshot("rounded", &sections(&parts));
}
