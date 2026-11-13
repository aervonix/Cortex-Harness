use cortex_base::{Rng, SeededRng};

use super::*;
use crate::cases::{self, ensure};
use crate::textfile::Ending;

fn edit(old: &str, new: &str) -> Edit {
    Edit {
        old: old.to_owned(),
        new: new.to_owned(),
        count: 1,
    }
}

fn times(old: &str, new: &str, count: usize) -> Edit {
    Edit {
        count,
        ..edit(old, new)
    }
}

/// Applies edits to a text, or says why not.
fn run(text: &str, edits: &[Edit]) -> Result<String, Vec<Failure>> {
    let parsed = Text::new(text);
    locate(&parsed, edits, &|| false).map(|places| apply(text, &places))
}

fn places(text: &str, edits: &[Edit]) -> Vec<Replacement> {
    locate(&Text::new(text), edits, &|| false).expect("every edit has its place")
}

fn one_failure(text: &str, edits: &[Edit]) -> Failure {
    let mut failures = run(text, edits).expect_err("the edits must be refused");
    assert_eq!(failures.len(), 1, "{failures:?}");
    failures.remove(0)
}

// --- The exact tier ---------------------------------------------------------

#[test]
fn an_exact_match_replaces_just_the_text_named() {
    let text = "fn main() {\n    println!(\"hi\");\n}\n";
    assert_eq!(
        run(text, &[edit("\"hi\"", "\"hello\"")]).unwrap(),
        "fn main() {\n    println!(\"hello\");\n}\n"
    );
    let found = places(text, &[edit("println!(\"hi\");\n}", "run();\n}")]);
    assert_eq!(found[0].lines, (2, 3));
    assert!(found[0].tolerant.is_none());
    assert_eq!(apply(text, &found), "fn main() {\n    run();\n}\n");
}

#[test]
fn text_copied_from_a_read_of_a_crlf_file_is_an_exact_match() {
    let text = "one\r\ntwo\r\nthree\r\n";
    // `read` shows these lines without their carriage returns.
    let found = places(text, &[edit("one\ntwo\n", "1\n2\n2b\n")]);
    assert!(found[0].tolerant.is_none(), "not a tolerant match");
    assert_eq!(found[0].range, 0..10, "the endings replaced are the file's");
    assert_eq!(apply(text, &found), "1\r\n2\r\n2b\r\nthree\r\n");

    // A match that begins and ends inside lines.
    assert_eq!(
        run(text, &[edit("ne\ntw", "NE\nTW")]).unwrap(),
        "oNE\r\nTWo\r\nthree\r\n"
    );
    // A line break that `new` adds is the file's kind.
    assert_eq!(
        run(text, &[edit("two", "two\ntwo and a half")]).unwrap(),
        "one\r\ntwo\r\ntwo and a half\r\nthree\r\n"
    );
    // And one that `new` writes as CRLF is not doubled.
    assert_eq!(
        run("a\nb\n", &[edit("a", "a\r\nc")]).unwrap(),
        "a\nc\nb\n",
        "a file with LF endings keeps them"
    );
}

#[test]
fn in_a_file_with_both_endings_new_lines_end_as_the_first_line_replaced_did() {
    let text = "a\r\nb\nc\r\nd\n";
    // The match starts on line 2, which ends with LF.
    assert_eq!(
        run(text, &[edit("b\nc", "B\nB2\nC")]).unwrap(),
        "a\r\nB\nB2\nC\r\nd\n"
    );
    // The match starts on line 1, which ends with CRLF.
    assert_eq!(
        run(text, &[edit("a\nb", "A\nB")]).unwrap(),
        "A\r\nB\nc\r\nd\n"
    );
    // A last line with no ending: the file's own kind (here a tie, which
    // the first line decides).
    assert_eq!(
        run("x\r\ny\nz", &[edit("z", "z\nend")]).unwrap(),
        "x\r\ny\nz\r\nend"
    );
}

#[test]
fn every_one_of_count_occurrences_is_replaced_and_any_other_number_is_refused() {
    let text = "let a = 1;\nlet b = 1;\nlet c = 1;\n";
    assert_eq!(
        run(text, &[times(" = 1;", " = 2;", 3)]).unwrap(),
        "let a = 2;\nlet b = 2;\nlet c = 2;\n"
    );
    assert_eq!(
        one_failure(text, &[edit(" = 1;", " = 2;")]),
        Failure::WrongCount {
            edit: 0,
            wanted: 1,
            at: vec![1, 2, 3],
            more: false,
            tolerant: false,
        }
    );
    assert!(matches!(
        one_failure(text, &[times(" = 1;", " = 2;", 4)]),
        Failure::WrongCount { wanted: 4, .. }
    ));
    // Very many: the search stops, and says that there are more.
    let many = "x\n".repeat(500);
    match one_failure(&many, &[edit("x", "y")]) {
        Failure::WrongCount { at, more, .. } => {
            assert!(more);
            assert_eq!(at.len(), 1 + EXTRA_OCCURRENCES);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn occurrences_that_share_text_are_refused() {
    assert_eq!(
        one_failure("aaa\n", &[times("aa", "b", 2)]),
        Failure::SelfOverlap {
            edit: 0,
            at: vec![1, 1]
        }
    );
    // Counted as two, so one is the wrong number too.
    assert!(matches!(
        one_failure("aaa\n", &[edit("aa", "b")]),
        Failure::WrongCount { at, .. } if at == [1, 1]
    ));
}

#[test]
fn edits_are_matched_against_the_file_as_it_was_and_must_not_overlap() {
    let text = "alpha\nbeta\ngamma\n";
    // The second edit's `old` is found in the original, not in what the
    // first edit wrote.
    assert_eq!(
        run(text, &[edit("alpha", "beta"), edit("beta\ng", "delta\ng")]).unwrap(),
        "beta\ndelta\ngamma\n"
    );
    // Whatever order they are given in.
    assert_eq!(
        run(text, &[edit("gamma", "c"), edit("alpha", "a")]).unwrap(),
        "a\nbeta\nc\n"
    );
    // Side by side is fine.
    assert_eq!(
        run(text, &[edit("alpha\n", "A\n"), edit("beta\n", "B\n")]).unwrap(),
        "A\nB\ngamma\n"
    );
    assert_eq!(
        one_failure(text, &[edit("alpha\nbe", "x"), edit("beta", "y")]),
        Failure::Overlap {
            first: 0,
            second: 1,
            first_lines: (1, 2),
            second_lines: (2, 2),
        }
    );
    // The same edit twice names the same text twice.
    assert!(matches!(
        one_failure(text, &[edit("beta", "x"), edit("beta", "y")]),
        Failure::Overlap {
            first: 0,
            second: 1,
            ..
        }
    ));
}

#[test]
fn every_failure_of_a_call_is_reported_and_nothing_is_applied() {
    let text = "one\ntwo\nthree\n";
    let failures = run(
        text,
        &[edit("one", "1"), edit("no such text", "x"), edit("t", "T")],
    )
    .expect_err("two of the three are wrong");
    assert_eq!(failures.len(), 2);
    assert!(matches!(failures[0], Failure::NotFound { edit: 1, .. }));
    assert!(matches!(failures[1], Failure::WrongCount { edit: 2, .. }));
}

#[test]
fn a_count_of_any_size_is_only_a_wrong_count() {
    // The count is the model's, and the search must not trip over it.
    for count in [usize::MAX, usize::MAX - 5, 1_000_000] {
        assert!(matches!(
            one_failure("a\nb\n", &[times("a", "x", count)]),
            Failure::WrongCount { wanted, tolerant: false, .. } if wanted == count
        ));
        // The tolerant tier, too.
        assert!(matches!(
            one_failure("  a\nb\n", &[times("a ", "x", count)]),
            Failure::WrongCount { wanted, tolerant: true, .. } if wanted == count
        ));
    }
}

#[test]
fn a_search_that_is_told_to_stop_stops_and_applies_nothing() {
    let text = "one\ntwo\n";
    let parsed = Text::new(text);
    assert_eq!(
        locate(&parsed, &[edit("one", "1")], &|| true),
        Err(vec![Failure::Stopped])
    );
    // The tolerant tier asks while it goes through a long file.
    let long = "x\n".repeat(3 * STOP_CHECK_EVERY);
    let parsed = Text::new(&long);
    let asked = std::cell::Cell::new(0);
    let stop = || {
        asked.set(asked.get() + 1);
        // Not at the start of the edit: later, inside the search.
        asked.get() > 2
    };
    assert_eq!(
        locate(&parsed, &[edit("  nowhere ", "y")], &stop),
        Err(vec![Failure::Stopped])
    );
    assert!(asked.get() >= 3, "asked {} times", asked.get());
}

#[test]
fn an_old_that_begins_with_blank_lines_is_found_among_many_blank_lines() {
    // Every gap of this file matches the blank start of `old`. One line
    // decides each place, so the search is quick, and finds the one place.
    let mut text = "\n".repeat(20_000);
    text.push_str("    the line\n");
    text.push_str(&"\n".repeat(20_000));
    let old = format!("{}the line  ", "\n".repeat(50));
    let found = places(&text, &[edit(&old, "replaced")]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].lines, (19_951, 20_001));
    assert!(found[0].tolerant.is_some());
}

#[test]
fn the_closest_text_is_worked_out_for_the_first_few_failures_only() {
    let text = "fn area() -> u32 {\n}\n";
    let edits: Vec<Edit> = (0..CLOSEST_MAX_EDITS + 2)
        .map(|number| edit(&format!("fn area() -> u{number} {{"), "x"))
        .collect();
    let failures = run(text, &edits).expect_err("none is in the file");
    assert_eq!(failures.len(), CLOSEST_MAX_EDITS + 2);
    for (index, failure) in failures.iter().enumerate() {
        let Failure::NotFound { closest, .. } = failure else {
            panic!("{failure:?}");
        };
        assert_eq!(closest.is_some(), index < CLOSEST_MAX_EDITS, "edit {index}");
    }
}

// --- The tolerant tier ------------------------------------------------------

#[test]
fn trailing_white_space_is_not_compared_by_the_tolerant_tier() {
    let text = "if ready {   \n    go();\t\n}\n";
    let found = places(text, &[edit("if ready {\n    go();\n}\n", "start();\n")]);
    let tolerant = found[0].tolerant.as_ref().expect("the tolerant tier");
    assert_eq!(tolerant.replaced, "if ready {   \n    go();\t\n}");
    assert_eq!(tolerant.reindented, "");
    assert_eq!(found[0].lines, (1, 3));
    assert_eq!(apply(text, &found), "start();\n");

    // The other way round: `old` has the stray white space.
    let found = places("a\nb\n", &[edit("a  \nb\t", "c\nd")]);
    assert!(found[0].tolerant.is_some());
    assert_eq!(apply("a\nb\n", &found), "c\nd\n");

    // A carriage return the model wrote into `old` is such white space.
    let found = places("a\nb\n", &[edit("a\r\nb\r\n", "c\n")]);
    assert!(found[0].tolerant.is_some());
    assert_eq!(apply("a\nb\n", &found), "c\n");
}

#[test]
fn a_uniform_difference_in_indentation_is_tolerated_and_given_to_the_new_text() {
    let text = "impl A {\n    fn f() {\n        work();\n    }\n}\n";
    // `old` was written with no indentation at all.
    let found = places(
        text,
        &[edit(
            "fn f() {\n    work();\n}",
            "fn f() {\n    work();\n\n    more();\n}",
        )],
    );
    let tolerant = found[0].tolerant.as_ref().expect("the tolerant tier");
    assert_eq!(tolerant.reindented, "4 spaces added to");
    assert_eq!(tolerant.replaced, "    fn f() {\n        work();\n    }");
    assert_eq!(
        apply(text, &found),
        "impl A {\n    fn f() {\n        work();\n\n        more();\n    }\n}\n",
        "a blank line of `new` stays blank"
    );

    // `old` was written with more indentation than the file has.
    let text = "fn f() {\n\twork();\n}\n";
    let found = places(
        text,
        &[edit(
            "\t\tfn f() {\n\t\t\twork();\n\t\t}",
            "\t\tfn g() {\n\t\t}",
        )],
    );
    assert_eq!(
        found[0].tolerant.as_ref().unwrap().reindented,
        "2 tabs taken off"
    );
    assert_eq!(apply(text, &found), "fn g() {\n}\n");
}

#[test]
fn a_difference_that_is_not_uniform_is_not_tolerated() {
    let text = "fn f() {\n    work();\n}\n";
    // Only the middle line is shifted.
    assert!(matches!(
        one_failure(text, &[edit("fn f() {\n        work();\n}", "x")]),
        Failure::NotFound { .. }
    ));
    // Tabs against spaces is another indentation, not more of the same.
    match one_failure(text, &[edit("fn f() {\n\twork();\n}", "x")]) {
        Failure::NotFound {
            closest: Some(closest),
            ..
        } => {
            assert_eq!(closest.lines, (1, 3));
            assert_eq!(
                closest.note.as_deref(),
                Some(
                    "Line 2 of the file is indented with 4 spaces, and the matching line of \
                     `old` with 1 tab."
                )
            );
        }
        other => panic!("{other:?}"),
    }
    // A blank line matches a blank line only.
    assert!(matches!(
        one_failure("a\n\nb\n", &[edit("a\nb", "x")]),
        Failure::NotFound { .. }
    ));
    // A difference inside a line is a different line.
    assert!(matches!(
        one_failure("a  b\n", &[edit("a b ", "x")]),
        Failure::NotFound { .. }
    ));
    // And `old` of blank lines only names nothing.
    assert!(matches!(
        one_failure("a\n\n\nb\n", &[edit("  \n", "x")]),
        Failure::NotFound { .. }
    ));
}

#[test]
fn the_tolerant_tier_is_used_only_when_the_exact_tier_finds_nothing() {
    // The text is in the file exactly once, and once more with other
    // indentation. The exact place is the one.
    let text = "    go();\ngo();\n";
    let found = places(text, &[edit("    go();", "    stop();")]);
    assert!(found[0].tolerant.is_none());
    assert_eq!(apply(text, &found), "    stop();\ngo();\n");
    // Asked for twice, it is in the file exactly once: the tolerant tier
    // does not make up the difference.
    assert!(matches!(
        one_failure(text, &[times("    go();", "x", 2)]),
        Failure::WrongCount {
            tolerant: false,
            ..
        }
    ));
}

#[test]
fn the_tolerant_tier_must_find_exactly_count_places() {
    let text = "  go();\n    go();\nstop();\n";
    assert_eq!(
        one_failure(text, &[edit("go(); ", "x")]),
        Failure::WrongCount {
            edit: 0,
            wanted: 1,
            at: vec![1, 2],
            more: false,
            tolerant: true,
        }
    );
    let found = places(text, &[times("go(); ", "run();", 2)]);
    assert_eq!(found.len(), 2);
    assert_eq!(apply(text, &found), "  run();\n    run();\nstop();\n");
    // Places that share lines are refused.
    assert!(matches!(
        one_failure("a \na \na \n", &[times("a\na\n", "x", 2)]),
        Failure::SelfOverlap { .. }
    ));
}

#[test]
fn new_text_that_cannot_take_the_same_change_of_indentation_is_refused() {
    let text = "go();\n";
    // `old` has four more spaces than the file. A `new` line with two
    // cannot have four taken off.
    assert_eq!(
        one_failure(text, &[edit("    go(); ", "    run();\n  odd();")]),
        Failure::CannotReindent {
            edit: 0,
            lines: (1, 1),
            extra: "4 spaces".to_owned(),
        }
    );
    // A blank line of `new` is no obstacle.
    let found = places(text, &[edit("    go(); ", "    run();\n\n    more();")]);
    assert_eq!(apply(text, &found), "run();\n\nmore();\n");
}

#[test]
fn whole_lines_replaced_keep_the_endings_of_the_lines_they_replace() {
    // The last line of a file with no final line break stays that way.
    let found = places("a\n  b", &[edit("b ", "c\nd")]);
    assert_eq!(apply("a\n  b", &found), "a\n  c\n  d");
    // Lines replaced by nothing go with their endings.
    let found = places("a\r\n  b  \r\nc\r\n", &[edit("    b", "")]);
    assert_eq!(apply("a\r\n  b  \r\nc\r\n", &found), "a\r\nc\r\n");
    // In a file with both kinds: the first replaced line's ending between
    // the new lines, the last replaced line's at the end.
    let text = "x\n  a\r\n  b\ny\r\n";
    let found = places(text, &[edit("a \nb ", "1\n2\n3")]);
    assert_eq!(apply(text, &found), "x\n  1\r\n  2\r\n  3\ny\r\n");
}

// --- What a failure shows ---------------------------------------------------

#[test]
fn the_closest_text_is_shown_with_how_it_differs() {
    let text =
        "fn area(w: u32, h: u32) -> u32 {\n    w * h\n}\n\nfn main() {\n    area(2, 3);\n}\n";
    match one_failure(
        text,
        &[edit("fn area(w: u32, h: u32) -> u64 {\n    w * h\n}", "x")],
    ) {
        Failure::NotFound {
            edit: 0,
            closest: Some(closest),
        } => {
            assert_eq!(closest.lines, (1, 3));
            assert_eq!(
                closest.diff,
                "@@ -1,3 +1,3 @@\n-fn area(w: u32, h: u32) -> u64 {\n+fn area(w: u32, h: u32) -> u32 {\n     w * h\n }"
            );
            assert_eq!(closest.note, None);
        }
        other => panic!("{other:?}"),
    }
    // No line in common: the line most like `old`'s longest.
    match one_failure(text, &[edit("    area(2, 4);", "x")]) {
        Failure::NotFound {
            closest: Some(closest),
            ..
        } => assert_eq!(closest.lines, (6, 6)),
        other => panic!("{other:?}"),
    }
    // Nothing like it anywhere.
    assert_eq!(
        one_failure(text, &[edit("completely different words", "x")]),
        Failure::NotFound {
            edit: 0,
            closest: None
        }
    );
    // A common line such as `}` alone does not make a region the closest.
    assert_eq!(
        one_failure(text, &[edit("zzz\n}", "x")]),
        Failure::NotFound {
            edit: 0,
            closest: None
        }
    );
}

#[test]
fn indentation_is_described_in_words() {
    assert_eq!(describe_indent(""), "no indentation");
    assert_eq!(describe_indent(" "), "1 space");
    assert_eq!(describe_indent("    "), "4 spaces");
    assert_eq!(describe_indent("\t"), "1 tab");
    assert_eq!(describe_indent("\t\t  "), "2 tabs and 2 spaces");
}

#[test]
fn the_lines_written_are_counted_in_the_new_text() {
    let text = "a\nb\nc\nd\ne\n";
    let found = places(
        text,
        &[
            edit("a", "a\na2\na3"),
            edit("c\n", ""),
            edit("e", "E"),
            edit("d\n", "d\nd2\n"),
        ],
    );
    let written = apply(text, &found);
    assert_eq!(written, "a\na2\na3\nb\nd\nd2\nE\n");
    assert_eq!(
        written_lines(text, &found),
        [(1, Some(3)), (5, None), (5, Some(6)), (7, Some(7))]
    );
}

// --- A whole new content over an old file -----------------------------------

#[test]
fn new_content_takes_the_line_endings_of_the_file_it_replaces() {
    let crlf = Text::new("one\r\ntwo\r\n");
    assert_eq!(rewrite(&crlf, "1\n2\n3\n"), "1\r\n2\r\n3\r\n");
    assert_eq!(
        rewrite(&crlf, "1\n2"),
        "1\r\n2",
        "no final break if none was given"
    );
    assert_eq!(rewrite(&crlf, "1\r\n2\r\n"), "1\r\n2\r\n", "not doubled");
    assert_eq!(rewrite(&crlf, ""), "");
    let lf = Text::new("one\ntwo\n");
    assert_eq!(
        rewrite(&lf, "1\r\n2\r\n"),
        "1\n2\n",
        "the file's kind, not the content's"
    );
    // A carriage return at the very end, with no line feed after it, is
    // not a line break: it is content, and is written.
    assert_eq!(rewrite(&crlf, "1\n2\r"), "1\r\n2\r");
    assert_eq!(rewrite(&lf, "1\r\n2\r"), "1\n2\r");
    // A file with no line break has no ending to keep: as given.
    assert_eq!(rewrite(&Text::new(""), "a\nb\n"), "a\nb\n");
    assert_eq!(rewrite(&Text::new("one line"), "a\r\nb\r\n"), "a\r\nb\r\n");
}

#[test]
fn a_carriage_return_that_ends_the_new_text_is_kept_by_both_tiers() {
    // The exact tier writes `new` as it is but for its line breaks.
    assert_eq!(
        run("a\nb\n", &[edit("b", "x\ny\r")]).unwrap(),
        "a\nx\ny\r\n"
    );
    // The tolerant tier writes whole lines, and keeps it too.
    let found = places("a\n  b\n", &[edit("b ", "x\ny\r")]);
    assert!(found[0].tolerant.is_some());
    assert_eq!(apply("a\n  b\n", &found), "a\n  x\n  y\r\n");
    // One written as part of a line break is the line break.
    let found = places("a\n  b\n", &[edit("b ", "x\r\ny\r\n")]);
    assert_eq!(apply("a\n  b\n", &found), "a\n  x\n  y\n");
}

#[test]
fn in_a_long_file_with_both_endings_a_write_still_keeps_the_endings_of_lines_it_keeps() {
    // More lines between the first change and the last than are compared
    // in full. Every line the write keeps still keeps its ending.
    let count = 6_000;
    let old: String = (0..count)
        .map(|n| format!("line {n}{}", if n % 3 == 0 { "\r\n" } else { "\n" }))
        .collect();
    let content: String = (0..count)
        .map(|n| match n {
            1 => "changed near the start\n".to_owned(),
            n if n == count - 2 => "changed near the end\n".to_owned(),
            n => format!("line {n}\n"),
        })
        .collect();
    let written = rewrite(&Text::new(&old), &content);
    let wanted: String = (0..count)
        .map(|n| match n {
            // A changed line gets the ending most of the file's lines have.
            1 => "changed near the start\n".to_owned(),
            n if n == count - 2 => "changed near the end\n".to_owned(),
            n => format!("line {n}{}", if n % 3 == 0 { "\r\n" } else { "\n" }),
        })
        .collect();
    assert!(written == wanted, "a kept line lost its ending");
}

#[test]
fn in_a_file_with_both_endings_the_lines_a_write_keeps_keep_theirs() {
    let mixed = Text::new("keep one\r\nold\nkeep two\nkeep three\r\nlast\r\n");
    assert_eq!(mixed.dominant, Ending::CrLf);
    assert_eq!(
        rewrite(
            &mixed,
            "keep one\nnew\nnewer\nkeep two\nkeep three\nlast\nadded\n"
        ),
        "keep one\r\nnew\r\nnewer\r\nkeep two\nkeep three\r\nlast\r\nadded\r\n"
    );
}

// --- Properties ---------------------------------------------------------------

/// A generated file: its lines, each with its ending.
#[derive(Debug, Clone)]
struct Lines(Vec<(String, Option<Ending>)>);

impl Lines {
    fn raw(&self) -> String {
        self.0
            .iter()
            .map(|(content, ending)| format!("{content}{}", ending.map_or("", Ending::as_str)))
            .collect()
    }

    /// Where a line's content starts in the raw text.
    fn start(&self, line: usize) -> usize {
        self.0[..line]
            .iter()
            .map(|(content, ending)| {
                content.len() + ending.map_or(0, |ending| ending.as_str().len())
            })
            .sum()
    }

    /// The ending of most lines, by the rule the tools state.
    fn dominant(&self) -> Ending {
        let crlf = self.count(Ending::CrLf);
        let lf = self.count(Ending::Lf);
        if crlf > lf || (crlf == lf && self.0.first().and_then(|line| line.1) == Some(Ending::CrLf))
        {
            Ending::CrLf
        } else {
            Ending::Lf
        }
    }

    fn count(&self, wanted: Ending) -> usize {
        self.0
            .iter()
            .filter(|(_, ending)| *ending == Some(wanted))
            .count()
    }
}

/// Which endings a generated file has.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Style {
    Lf,
    CrLf,
    Mixed,
}

const WORDS: &[&str] = &[
    "let", "x", "=", "y", "+", "1;", "fn", "go()", "{", "}", "é", "日本", "if", "else", "//",
];

/// A file of up to twelve lines. Every line holds a word no other line
/// has, so that a piece of text which takes in such a word is in the file
/// once.
fn lines(rng: &mut SeededRng, style: Style) -> Lines {
    let count = rng.below(12) as usize + 1;
    let final_break = rng.below(4) != 0;
    let mut lines = Vec::with_capacity(count);
    for number in 0..count {
        let indent = " ".repeat(rng.below(3) as usize * 4);
        let mut content = format!("{indent}<{number}>");
        for _ in 0..rng.below(4) {
            content.push(' ');
            content.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
        }
        let ending = match style {
            Style::Lf => Ending::Lf,
            Style::CrLf => Ending::CrLf,
            Style::Mixed if rng.below(2) == 0 => Ending::Lf,
            Style::Mixed => Ending::CrLf,
        };
        let last = number + 1 == count;
        lines.push((content, (!last || final_break).then_some(ending)));
    }
    Lines(lines)
}

fn style(rng: &mut SeededRng) -> Style {
    match rng.below(3) {
        0 => Style::Lf,
        1 => Style::CrLf,
        _ => Style::Mixed,
    }
}

/// A piece of a file picked by lines and columns, with what replaces it.
#[derive(Debug, Clone)]
struct Pick {
    first: usize,
    last: usize,
    /// Byte columns: where the piece starts in its first line and ends in
    /// its last.
    from: usize,
    to: usize,
    new: String,
}

impl Pick {
    /// The piece as `read` shows it.
    fn old(&self, lines: &Lines) -> String {
        if self.first == self.last {
            return lines.0[self.first].0[self.from..self.to].to_owned();
        }
        let mut old = lines.0[self.first].0[self.from..].to_owned();
        for line in &lines.0[self.first + 1..self.last] {
            old.push('\n');
            old.push_str(&line.0);
        }
        old.push('\n');
        old.push_str(&lines.0[self.last].0[..self.to]);
        old
    }
}

/// A column of `text` that is between characters.
fn column(rng: &mut SeededRng, text: &str) -> usize {
    let mut at = rng.below(text.len() as u64 + 1) as usize;
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Up to three pieces of a file, in order and apart, each of which takes
/// in the word that marks its first line, with new text that is in the file
/// nowhere else.
fn picks(rng: &mut SeededRng, lines: &Lines) -> Vec<Pick> {
    let mut picks = Vec::new();
    let mut line = 0;
    while line < lines.0.len() && picks.len() < 3 {
        if rng.below(3) == 0 {
            line += 1;
            continue;
        }
        let first = line;
        let last = (first + rng.below(3) as usize).min(lines.0.len() - 1);
        let content = &lines.0[first].0;
        let mark = content.find('<').expect("every line has its mark");
        // From somewhere up to the mark...
        let from = column(rng, &content[..mark]);
        // ...to somewhere past it, or into the last line.
        let to = if first == last {
            let close = content.find('>').expect("every line has its mark") + 1;
            close + column(rng, &content[close..])
        } else {
            column(rng, &lines.0[last].0)
        };
        let mut new = format!("[{}]", picks.len());
        for _ in 0..rng.below(3) {
            new.push_str(if rng.below(2) == 0 { "\n" } else { " word" });
        }
        picks.push(Pick {
            first,
            last,
            from,
            to,
            new,
        });
        // Leave a line between two pieces.
        line = last + 2;
    }
    picks
}

/// What the file must hold after the picks are replaced, worked out from
/// the lines themselves: everything outside a piece byte for byte, and
/// each line break of the new text as the ending of the piece's first
/// line.
fn expected(lines: &Lines, picks: &[Pick]) -> String {
    let raw = lines.raw();
    let mut result = String::new();
    let mut at = 0;
    for pick in picks {
        let start = lines.start(pick.first) + pick.from;
        let end = lines.start(pick.last) + pick.to;
        result.push_str(&raw[at..start]);
        let ending = lines.0[pick.first].1.unwrap_or_else(|| lines.dominant());
        result.push_str(&pick.new.replace('\n', ending.as_str()));
        at = end;
    }
    result.push_str(&raw[at..]);
    result
}

#[test]
fn property_text_outside_an_edit_is_byte_identical_whatever_the_endings() {
    cases::check(
        "matching_endings_survive",
        400,
        |rng| {
            let lines = {
                let style = style(rng);
                lines(rng, style)
            };
            let picks = picks(rng, &lines);
            (lines, picks)
        },
        |(lines, picks)| {
            let raw = lines.raw();
            let edits: Vec<Edit> = picks
                .iter()
                .map(|pick| edit(&pick.old(lines), &pick.new))
                .collect();
            let parsed = Text::new(&raw);
            let places =
                locate(&parsed, &edits, &|| false).map_err(|failures| format!("{failures:?}"))?;
            ensure!(
                places.iter().all(|place| place.tolerant.is_none()),
                "an exact piece was matched by the tolerant tier"
            );
            let written = apply(&raw, &places);
            let wanted = expected(lines, picks);
            ensure!(
                written == wanted,
                "written {written:?}\n  wanted  {wanted:?}"
            );
            Ok(())
        },
    );
}

#[test]
fn property_applying_and_then_reversing_restores_the_original() {
    cases::check(
        "matching_reverse",
        400,
        |rng| {
            // One kind of ending: in a file with both, text written over a
            // piece that spans two kinds cannot say which was where.
            let style = if rng.below(2) == 0 {
                Style::Lf
            } else {
                Style::CrLf
            };
            let lines = lines(rng, style);
            let picks = picks(rng, &lines);
            (lines, picks)
        },
        |(lines, picks)| {
            let raw = lines.raw();
            let forward: Vec<Edit> = picks
                .iter()
                .map(|pick| edit(&pick.old(lines), &pick.new))
                .collect();
            let changed = run(&raw, &forward).map_err(|failures| format!("{failures:?}"))?;
            if raw.contains("\r\n") && !changed.contains("\r\n") {
                // Every line break of the file was replaced away. What is
                // left has no line ending to go by, so text written into it
                // gets the default, and the original's kind is not known.
                return Ok(());
            }
            let backward: Vec<Edit> = picks
                .iter()
                .map(|pick| edit(&pick.new, &pick.old(lines)))
                .collect();
            let restored =
                run(&changed, &backward).map_err(|failures| format!("back: {failures:?}"))?;
            ensure!(
                restored == raw,
                "restored {restored:?}\n  original {raw:?}\n  between  {changed:?}"
            );
            Ok(())
        },
    );
}

/// A run of whole lines, and an `old` for it that is disturbed in the ways
/// the tolerant tier allows.
#[derive(Debug, Clone)]
struct Disturbed {
    first: usize,
    last: usize,
    old: String,
    new: Vec<String>,
    /// Spaces added in front of every line of `old` (and of `new`).
    added: usize,
    /// Spaces taken off the front of every line of `old` (and of `new`).
    removed: usize,
    /// Whether `old` is, after all that, exactly what the file has.
    exact: bool,
}

fn disturbed(rng: &mut SeededRng, lines: &Lines) -> Disturbed {
    let first = rng.below(lines.0.len() as u64) as usize;
    let last = (first + rng.below(3) as usize).min(lines.0.len() - 1);
    let chosen: Vec<&str> = lines.0[first..=last]
        .iter()
        .map(|line| line.0.as_str())
        .collect();
    let shallowest = chosen
        .iter()
        .map(|line| line.len() - line.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let (added, removed) = match rng.below(3) {
        0 => (0, 0),
        1 => (rng.below(2) as usize * 2 + 2, 0),
        _ => (0, shallowest),
    };
    let mut trailing = false;
    let old: Vec<String> = chosen
        .iter()
        .map(|line| {
            let mut line = format!("{}{}", " ".repeat(added), &line[removed..]);
            if rng.below(3) == 0 {
                line.push_str(if rng.below(2) == 0 { "  " } else { "\t" });
                trailing = true;
            }
            line
        })
        .collect();
    // `new` is written as `old` is: with the same indentation in front.
    let base = chosen[0].len() - chosen[0].trim_start_matches(' ').len() + added - removed;
    let new: Vec<String> = (0..rng.below(3) + 1)
        .map(|number| format!("{}[new {number}]", " ".repeat(base)))
        .collect();
    Disturbed {
        first,
        last,
        old: old.join("\n"),
        new,
        added,
        removed,
        exact: added == 0 && removed == 0 && !trailing,
    }
}

#[test]
fn property_the_tolerant_tier_is_reported_whenever_it_is_what_matched() {
    cases::check(
        "matching_tolerant_reported",
        400,
        |rng| {
            let lines = {
                let style = style(rng);
                lines(rng, style)
            };
            let disturbed = disturbed(rng, &lines);
            (lines, disturbed)
        },
        |(lines, disturbed)| {
            let raw = lines.raw();
            let parsed = Text::new(&raw);
            let new = disturbed.new.join("\n");
            let places = locate(&parsed, &[edit(&disturbed.old, &new)], &|| false)
                .map_err(|failures| format!("{failures:?}"))?;
            ensure!(places.len() == 1, "{} places", places.len());
            let place = &places[0];
            // Whether `old` is in the file as `read` shows it. One line
            // with its indentation taken off still is: it is a piece of
            // its line.
            let in_file = raw.replace("\r\n", "\n").contains(&disturbed.old);
            ensure!(
                place.tolerant.is_none() == in_file,
                "in the file exactly: {in_file}, reported as tolerant: {}",
                place.tolerant.is_some()
            );
            ensure!(
                !disturbed.exact || in_file,
                "an undisturbed `old` was not found exactly"
            );
            if in_file && !disturbed.exact {
                // A piece of a line, replaced as written. The exact tier's
                // own properties cover what that writes.
                return Ok(());
            }
            ensure!(
                place.lines.0 == disturbed.first + 1,
                "matched at line {}",
                place.lines.0
            );
            if let Some(tolerant) = &place.tolerant {
                // What it says was replaced is what the file had there.
                let had: Vec<&str> = lines.0[disturbed.first..=disturbed.last]
                    .iter()
                    .map(|line| line.0.as_str())
                    .collect();
                ensure!(
                    tolerant.replaced == had.join("\n"),
                    "reported {:?}, the file had {had:?}",
                    tolerant.replaced
                );
                ensure!(
                    tolerant.reindented.is_empty() == (disturbed.added == disturbed.removed),
                    "reindented: {:?}",
                    tolerant.reindented
                );
            }
            // The new lines are written with the file's indentation, and
            // every other line of the file is as it was.
            let written = apply(&raw, &places);
            let mut wanted = lines.clone();
            let ending = wanted.0[disturbed.first]
                .1
                .unwrap_or_else(|| lines.dominant());
            let last_ending = wanted.0[disturbed.last].1;
            let count = disturbed.new.len();
            let replaced: Vec<(String, Option<Ending>)> = disturbed
                .new
                .iter()
                .enumerate()
                .map(|(number, line)| {
                    let line = if disturbed.exact {
                        line.clone()
                    } else {
                        format!(
                            "{}{}",
                            " ".repeat(disturbed.removed),
                            &line[disturbed.added.min(line.len())..]
                        )
                    };
                    let ending = if number + 1 == count {
                        last_ending
                    } else {
                        Some(ending)
                    };
                    (line, ending)
                })
                .collect();
            wanted.0.splice(disturbed.first..=disturbed.last, replaced);
            ensure!(
                written == wanted.raw(),
                "written {written:?}\n  wanted  {:?}",
                wanted.raw()
            );
            Ok(())
        },
    );
}

#[test]
fn property_ambiguous_and_overlapping_edits_are_refused() {
    cases::check(
        "matching_refusals",
        400,
        |rng| {
            let mut lines = {
                let style = style(rng);
                lines(rng, style)
            };
            // A line that is in the file twice.
            let copy = lines.0[0].clone();
            let at = rng.below(lines.0.len() as u64 + 1) as usize;
            let ending = copy.1.or(Some(Ending::Lf));
            if let Some(last) = lines.0.last_mut() {
                last.1 = last.1.or(Some(Ending::Lf));
            }
            lines.0.insert(at.max(1), (copy.0, ending));
            let picks = picks(rng, &lines);
            (lines, picks, rng.below(3))
        },
        |(lines, picks, kind)| {
            let raw = lines.raw();
            let twice = lines.0[0].0.clone();
            let mut edits: Vec<Edit> = picks
                .iter()
                .filter(|pick| pick.first > 0 && lines.0[pick.first].0 != twice)
                .map(|pick| edit(&pick.old(lines), &pick.new))
                .collect();
            match kind {
                // Text that is in the file twice, named as if once.
                0 => edits.push(edit(&twice, "once")),
                // And with the wrong number.
                1 => edits.push(times(&twice, "thrice", 3)),
                // Two edits for the same text.
                _ => {
                    edits.push(times(&twice, "first", 2));
                    edits.push(times(twice.trim_start(), "second", 2));
                }
            }
            match run(&raw, &edits) {
                Ok(written) => Err(format!("applied, giving {written:?}").into()),
                Err(failures) => {
                    let wanted = failures.iter().any(|failure| match (kind, failure) {
                        (0, Failure::WrongCount { wanted: 1, at, .. })
                        | (1, Failure::WrongCount { wanted: 3, at, .. }) => at.len() == 2,
                        (2, Failure::Overlap { .. }) => true,
                        _ => false,
                    });
                    ensure!(wanted, "refused for another reason: {failures:?}");
                    Ok(())
                }
            }
        },
    );
}
