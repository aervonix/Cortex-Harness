use std::collections::BTreeMap;

use cortex_base::Rng as _;
use cortex_testkit::cases::{Outcome, check, ensure};

use super::*;

/// The pieces as `text` with marks: `*bold*`-like brackets are not used,
/// so that the source and the result are not confused: `[b]` bold, `[i]`
/// italic, `[c]` code, `[l:url]` a link.
fn shown(source: &str) -> String {
    let parsed = parse(source, &[]);
    let mut out = String::new();
    for piece in &parsed.pieces {
        let mut tags = Vec::new();
        if piece.marks.bold {
            tags.push("b".to_owned());
        }
        if piece.marks.italic {
            tags.push("i".to_owned());
        }
        if piece.marks.code {
            tags.push("c".to_owned());
        }
        if let Some(url) = &piece.link {
            tags.push(format!("l:{url}"));
        }
        if tags.is_empty() {
            out.push_str(&piece.text);
        } else {
            out.push_str(&format!("[{}]{}[/]", tags.join(" "), piece.text));
        }
    }
    out
}

#[test]
fn code_spans_emphasis_and_links() {
    assert_eq!(shown("plain text"), "plain text");
    assert_eq!(shown("a `b c` d"), "a [c]b c[/] d");
    assert_eq!(shown("`` a`b ``"), "[c]a`b[/]");
    assert_eq!(shown("*em* and **strong**"), "[i]em[/] and [b]strong[/]");
    assert_eq!(shown("***both***"), "[b i]both[/]");
    assert_eq!(shown("_em_ snake_case_name"), "[i]em[/] snake_case_name");
    assert_eq!(shown("**a *b* c**"), "[b]a [/][b i]b[/][b] c[/]");
    assert_eq!(
        shown("see [the docs](https://e.example/x \"Title\") now"),
        "see [l:https://e.example/x]the docs[/] now"
    );
    assert_eq!(shown("<https://e.example>"), "[l:https://e.example]https://e.example[/]");
    assert_eq!(shown("![a cat](cat.png)"), "[l:cat.png]a cat[/]");
    assert_eq!(shown("[*em* link](u)"), "[i l:u]em[/][l:u] link[/]");
    // Emphasis does not pair across a link's edge.
    assert_eq!(shown("*a [b* c](u)"), "*a [l:u]b* c[/]");
    assert_eq!(shown("[a [b](u) c](v)"), "[a [l:u]b[/] c](v)");
}

#[test]
fn escapes_entities_and_breaks() {
    assert_eq!(shown(r"\*not em\*"), "*not em*");
    assert_eq!(shown("&amp; &copy; &#65; &#x41; &bogus;"), "& © A A &bogus;");
    // A control character from an entity is made visible, as content's are.
    assert_eq!(shown("a&#27;b &#0;"), "a\u{241b}b \u{fffd}");
    assert_eq!(shown("a\nb"), "a b");
    assert_eq!(shown("a  \nb"), "a\nb");
    assert_eq!(shown("a\\\nb"), "a\nb");
    assert_eq!(shown("tab\there"), "tab here");
    assert_eq!(shown("a < b > c"), "a < b > c");
}

#[test]
fn the_marks_still_open_are_where_they_are() {
    let opens = |source: &str| parse(source, &[]).opens;
    assert_eq!(opens("plain"), Vec::<usize>::new());
    assert_eq!(opens("a *b"), [2]);
    assert_eq!(opens("a **b *c"), [2, 5]);
    assert_eq!(opens("a `b"), [2]);
    assert_eq!(opens("x [y"), [2]);
    assert_eq!(opens("[y](https://e"), [0]);
    // Marks that cannot open are not open.
    assert_eq!(opens("a * b"), Vec::<usize>::new());
    assert_eq!(opens("foo*"), Vec::<usize>::new());
    assert_eq!(opens("snake_case"), Vec::<usize>::new());
    // Matched marks are not open.
    assert_eq!(opens("*a* `b` [c](d)"), Vec::<usize>::new());
    // A frozen mark is text, and not open.
    let frozen = parse("*a b* c", &[0]);
    assert_eq!(frozen.opens, Vec::<usize>::new());
    assert!(frozen.pieces.iter().all(|piece| !piece.marks.italic));
    assert_eq!(frozen.pieces[0].text, "*a b* c");
}

#[test]
fn every_piece_knows_its_source() {
    let source = "a `code` **b** [l](u) &amp; \\* c";
    let parsed = parse(source, &[]);
    for piece in &parsed.pieces {
        if piece.linear {
            assert_eq!(piece.text.len(), piece.src.len(), "{piece:?}");
            assert_eq!(piece.text, source[piece.src.clone()].replace('\t', " "), "{piece:?}");
        }
    }
    let code = parsed.pieces.iter().find(|piece| piece.marks.code).unwrap();
    assert_eq!(&source[code.src.clone()], "code");
    assert_eq!(code.depth, 1);
    let bold = parsed.pieces.iter().find(|piece| piece.marks.bold).unwrap();
    assert_eq!(bold.depth, 1);
    assert_eq!(parsed.pieces[0].depth, 0);
}

/// Text that is mostly markup.
fn markdownish(rng: &mut cortex_base::SeededRng) -> String {
    const PARTS: [&str; 22] = [
        "*", "**", "_", "__", "`", "``", "[", "]", "(", ")", "<", ">", "&amp;", "&", "\\", " ",
        " ", "\n", "word", "x", "https://e.example", "漢",
    ];
    let count = rng.below(24);
    let mut text = String::new();
    for _ in 0..count {
        text.push_str(PARTS[usize::try_from(rng.below(PARTS.len() as u64)).unwrap()]);
    }
    text
}

/// Each source byte a parse shows something for, and what: the shown
/// character of a linear piece, or a whole non-linear piece at its start.
fn by_source(parsed: &Parsed) -> BTreeMap<usize, (String, Marks, Option<String>)> {
    let mut map = BTreeMap::new();
    for piece in &parsed.pieces {
        if piece.linear {
            for (offset, c) in piece.text.char_indices() {
                map.insert(piece.src.start + offset, (c.to_string(), piece.marks, piece.link.clone()));
            }
        } else {
            map.insert(piece.src.start, (piece.text.clone(), piece.marks, piece.link.clone()));
        }
    }
    map
}

#[test]
fn nothing_before_the_first_open_mark_changes_when_text_is_added() {
    check(
        "nothing before the first open mark changes when text is added",
        400,
        markdownish,
        |text| -> Outcome {
            let full = by_source(&parse(text, &[]));
            // Every end of a word: the places the paragraph's rows are
            // decided at.
            for (end, c) in text.char_indices() {
                if !c.is_whitespace() || end == 0 || text[..end].ends_with(char::is_whitespace) {
                    continue;
                }
                let prefix = parse(&text[..end], &[]);
                let open = prefix.open().unwrap_or(end);
                for (at, shown) in by_source(&prefix) {
                    if at >= open {
                        break;
                    }
                    ensure!(
                        full.get(&at) == Some(&shown),
                        "at {at} of {:?}, the prefix shows {shown:?} and the whole {:?}",
                        &text[..end],
                        full.get(&at)
                    );
                }
            }
            Ok(())
        },
    );
}

#[test]
fn after_its_open_marks_are_frozen_nothing_in_a_prefix_changes() {
    check(
        "after its open marks are frozen nothing in a prefix changes",
        400,
        markdownish,
        |text| -> Outcome {
            for (end, c) in text.char_indices() {
                if !c.is_whitespace() || end == 0 || text[..end].ends_with(char::is_whitespace) {
                    continue;
                }
                let frozen = parse(&text[..end], &[]).opens;
                let prefix = parse(&text[..end], &frozen);
                // Freezing what was open changes nothing in the prefix…
                ensure!(
                    by_source(&prefix) == by_source(&parse(&text[..end], &[])),
                    "freezing changed {:?}",
                    &text[..end]
                );
                // …and nothing that follows can change it.
                let full = by_source(&parse(text, &frozen));
                for (at, shown) in by_source(&prefix) {
                    ensure!(
                        full.get(&at) == Some(&shown),
                        "at {at} of {:?}, the prefix shows {shown:?} and the whole {:?}",
                        &text[..end],
                        full.get(&at)
                    );
                }
            }
            Ok(())
        },
    );
}

#[test]
fn pieces_are_in_order_and_linear_ones_are_their_source() {
    check(
        "pieces are in order and linear ones are their source",
        400,
        markdownish,
        |text| -> Outcome {
            let parsed = parse(text, &[]);
            let mut at = 0;
            for piece in &parsed.pieces {
                ensure!(piece.src.start >= at, "{piece:?} goes back before {at}");
                ensure!(piece.src.end <= text.len(), "{piece:?} is past the end");
                ensure!(!piece.text.is_empty(), "an empty piece");
                if piece.linear {
                    ensure!(
                        piece.text == text[piece.src.clone()].replace(['\t', '\n'], " "),
                        "{piece:?} is not its source"
                    );
                }
                ensure!(
                    !piece.text.contains(|c: char| c != '\n' && c.is_control()),
                    "{piece:?} holds a control character"
                );
                at = piece.src.end;
            }
            Ok(())
        },
    );
}
