use cortex_base::Rng as _;
use cortex_base::text::graphemes;
use cortex_term::Color;
use cortex_testkit::cases::{Outcome, check, ensure};

use super::*;
use crate::testing::tricky_text;

fn red() -> Style {
    Style::PLAIN.fg(Color::Ansi(1))
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(|line| line.text().into_owned()).collect()
}

/// The clusters of a text, as strings.
fn clusters(text: &str) -> Vec<&str> {
    graphemes(text).map(|(_, cluster, _)| cluster).collect()
}

#[test]
fn wrapping_keeps_each_spans_style_across_lines() {
    let spans = [
        Span::plain("scan_number stops at the "),
        Span::new("first character", red()),
        Span::plain(" that is not a digit"),
    ];
    let lines = wrap(&spans, 20);
    assert_eq!(
        texts(&lines),
        [
            "scan_number stops at",
            "the first character",
            "that is not a digit"
        ]
    );
    assert_eq!(lines[1].spans()[1].text(), "first character");
    assert_eq!(lines[1].spans()[1].style(), red());
    assert_eq!(wrap(&[], 10), vec![Line::new()]);
    // A link survives the wrap.
    let linked = wrap(&[Span::plain("see the docs").link("https://e.example/")], 7);
    assert_eq!(texts(&linked), ["see the", "docs"]);
    assert!(
        linked
            .iter()
            .all(|line| line.spans()[0].url() == Some("https://e.example/"))
    );
}

#[test]
fn a_cut_ends_in_the_ellipsis_in_the_style_before_it() {
    let line = Line::from(vec![Span::plain("abc"), Span::new("defgh", red())]);
    assert_eq!(cut(&line, 8, "…"), line);
    let short = cut(&line, 6, "…");
    assert_eq!(short.text(), "abcde…");
    assert_eq!(short.spans().last().unwrap().style(), red());
    assert_eq!(cut(&line, 4, "…").text(), "abc…");
    assert_eq!(
        cut(&line, 4, "…").spans().last().unwrap().style(),
        Style::PLAIN
    );
    assert_eq!(cut(&line, 6, "...").text(), "abc...");
    assert_eq!(cut(&line, 2, "...").text(), "..");
    assert_eq!(cut(&line, 0, "…").text(), "");
    // A wide character that does not fit whole is left out.
    let wide = Line::plain("ab漢字");
    assert_eq!(cut(&wide, 5, "…").text(), "ab漢…");
    assert_eq!(cut(&wide, 4, "…").text(), "ab…");
    assert_eq!(cut(&wide, 4, "…").width(), 3);
}

#[test]
fn spread_puts_the_sides_at_the_ends_and_cuts_the_left() {
    let left = Line::plain("esc stops · enter steers this turn");
    let right = Line::plain("opus-5.5 high");
    let row = spread(&left, &right, 60, 2, "…");
    assert_eq!(row.width(), 60);
    assert!(row.text().ends_with("opus-5.5 high"));
    assert!(row.text().starts_with("esc stops"));
    let narrow = spread(&left, &right, 30, 2, "…");
    assert_eq!(narrow.text(), "esc stops · en…  opus-5.5 high");
    assert_eq!(narrow.width(), 30);
    // No room for the right side: the left has the row.
    assert_eq!(spread(&left, &right, 12, 2, "…").text(), "esc stops ·…");
    assert_eq!(pad(Line::plain("ab"), 1, red()).text(), "ab");
    assert_eq!(pad(Line::plain("ab"), 4, red()).spans()[1].style(), red());
}

#[test]
fn wrapping_never_splits_a_cluster_or_passes_the_width() {
    check(
        "wrapping never splits a cluster or passes the width",
        300,
        |rng| {
            let parts = 1 + rng.below(3);
            let spans: Vec<String> = (0..parts).map(|_| tricky_text(rng, 8)).collect();
            (spans, 1 + rng.below(30))
        },
        |(parts, width)| -> Outcome {
            let width = usize::try_from(*width).unwrap();
            let spans: Vec<Span> = parts.iter().map(Span::plain).collect();
            let whole: String = parts.concat();
            let lines = wrap(&spans, width);
            let mut rest = whole.as_str();
            for line in &lines {
                let text = line.text();
                // A line is wider only when it is a single cluster.
                ensure!(
                    line.width() <= width || clusters(&text).len() == 1,
                    "{text:?} is {} columns, over {width}",
                    line.width()
                );
                // Each line's text is found next in the original, starting on a
                // cluster boundary, with only white space and line breaks
                // skipped before it.
                let at = rest.find(text.as_ref()).unwrap_or(usize::MAX);
                ensure!(at != usize::MAX, "{text:?} is not next in {rest:?}");
                ensure!(
                    rest[..at].chars().all(char::is_whitespace),
                    "{:?} was dropped before {text:?}",
                    &rest[..at]
                );
                let start = whole.len() - rest.len() + at;
                let boundaries: Vec<usize> =
                    graphemes(&whole).map(|(offset, _, _)| offset).collect();
                ensure!(
                    text.is_empty() || boundaries.contains(&start),
                    "{text:?} starts inside a cluster"
                );
                let end = start + text.len();
                ensure!(
                    end == whole.len() || boundaries.contains(&end),
                    "{text:?} ends inside a cluster"
                );
                rest = &rest[at + text.len()..];
            }
            ensure!(
                rest.chars().all(char::is_whitespace),
                "{rest:?} was lost at the end"
            );
            Ok(())
        },
    );
}

#[test]
fn a_cut_never_splits_a_cluster_or_passes_the_width() {
    check(
        "a cut never splits a cluster or passes the width",
        300,
        |rng| {
            let parts = 1 + rng.below(3);
            let spans: Vec<String> = (0..parts).map(|_| tricky_text(rng, 8)).collect();
            (spans, rng.below(30), rng.below(2) == 0)
        },
        |(parts, width, ascii)| -> Outcome {
            let width = usize::try_from(*width).unwrap();
            let mark = if *ascii { "..." } else { "…" };
            let line = Line::from(parts.iter().map(Span::plain).collect::<Vec<_>>());
            let whole = line.text().into_owned();
            let out = cut(&line, width, mark);
            ensure!(out.width() <= width, "{:?} is over {width}", out.text());
            let text = out.text().into_owned();
            if line.width() <= width {
                ensure!(
                    text == whole,
                    "{whole:?} fitted and was changed to {text:?}"
                );
                return Ok(());
            }
            if text.is_empty() || !text.ends_with(mark) {
                ensure!(
                    mark.starts_with(&text),
                    "{text:?} is not part of the ellipsis"
                );
                return Ok(());
            }
            let kept = &text[..text.len() - mark.len()];
            ensure!(
                whole.starts_with(kept),
                "{kept:?} is not the start of {whole:?}"
            );
            let original = clusters(&whole);
            ensure!(
                clusters(kept).iter().zip(&original).all(|(a, b)| a == b),
                "{kept:?} splits a cluster of {whole:?}"
            );
            ensure!(
                original.join("").starts_with(kept)
                    && graphemes(&whole).any(|(offset, _, _)| offset == kept.len()),
                "{kept:?} ends inside a cluster"
            );
            Ok(())
        },
    );
}
