//! Line diffs between two versions of a file, for changed-only re-reads
//! (`docs/architecture/15-efficiency.md`).
//!
//! The diffing itself is the `similar` crate's. This module turns its result
//! into the text a model is shown, and into the answer to a question only
//! this harness asks: after seeing this diff, which lines of the new version
//! has the model seen?

use std::fmt::Write as _;

use similar::{Algorithm, DiffOp, capture_diff_slices, group_diff_ops};

use crate::files::LineRanges;

/// How many unchanged lines are shown around each change.
const CONTEXT: usize = 3;

/// Two versions with more lines than this between them are not diffed: the
/// file has "changed a lot", and its content is sent.
pub(crate) const MAX_LINES: usize = 50_000;

/// The difference between two versions of a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diff {
    /// The hunks, in unified form: `@@ -a,b +c,d @@` and the lines under
    /// it. Empty when the versions have the same lines, which happens when
    /// only line endings or the final line break changed.
    pub(crate) text: String,
    /// How many lines the new version has that the old did not.
    pub(crate) added: usize,
    /// How many lines the old version had that the new does not.
    pub(crate) removed: usize,
    ops: Vec<DiffOp>,
    hunks: Vec<Vec<DiffOp>>,
}

/// The lines of a text as `read` numbers and shows them: split at line
/// feeds, without the carriage return of a CRLF ending or a byte-order mark
/// at the start, and with a final line break ending the last line, not
/// starting another.
pub(crate) fn lines_of(text: &str) -> Vec<&str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// Diffs `old` against `new`, line by line. `None` when the two are too
/// large to diff.
pub(crate) fn diff(old: &str, new: &str) -> Option<Diff> {
    let old_lines = lines_of(old);
    let new_lines = lines_of(new);
    if old_lines.len() + new_lines.len() > MAX_LINES {
        return None;
    }
    let ops = capture_diff_slices(Algorithm::Myers, &old_lines, &new_lines);
    Some(render(&old_lines, &new_lines, ops))
}

/// Between two versions that an editing tool has just made, the lines that
/// differ may take this many lines of work to compare, once the lines the
/// versions share at their start and at their end are set aside. Past it,
/// everything between is shown as replaced.
///
/// The comparison's worst case grows with the square of this, and it is
/// met by a file that was written again from top to bottom: at 4,000 lines
/// that is some tens of milliseconds, on the path of a tool call.
const MIDDLE_MAX_LINES: usize = 4_000;

/// [`diff`] for a change the harness made itself, with a bound on the work.
///
/// The lines two versions share at their start and their end are found
/// first, which costs one pass and is all there is to find in most edits.
/// Only what lies between is compared in full, and only when it is small.
/// `None` when the versions are too large to show a diff of at all.
pub(crate) fn diff_bounded(old: &str, new: &str) -> Option<Diff> {
    let old_lines = lines_of(old);
    let new_lines = lines_of(new);
    if old_lines.len() + new_lines.len() > MAX_LINES {
        return None;
    }
    let ops = bounded_ops(&old_lines, &new_lines);
    Some(render(&old_lines, &new_lines, ops))
}

/// Runs of lines `old` and `new` have in common, in the order of both: for
/// each, where it starts in `old`, where in `new`, and how many lines it
/// has.
///
/// They are the runs of [`diff_bounded`]'s comparison. Where that gave up
/// on a middle too large to compare in full, the lines of the middle are
/// paired by a single pass instead ([`paired_runs`]): not always as many
/// pairs as there are, and every pair is the same line in both.
pub(crate) fn common_runs(old: &[&str], new: &[&str]) -> Vec<(usize, usize, usize)> {
    let mut runs = Vec::new();
    for op in bounded_ops(old, new) {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => runs.push((old_index, new_index, len)),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } if old_len + new_len > MIDDLE_MAX_LINES => {
                let paired = paired_runs(
                    &old[old_index..old_index + old_len],
                    &new[new_index..new_index + new_len],
                );
                runs.extend(
                    paired
                        .into_iter()
                        .map(|(in_old, in_new, len)| (old_index + in_old, new_index + in_new, len)),
                );
            }
            _ => {}
        }
    }
    runs
}

/// How far ahead in the old lines a new line may find its pair. A line
/// such as `}` is everywhere: without a limit, one early pairing far ahead
/// would leave everything between unpaired.
const PAIR_WINDOW: usize = 256;

/// Pairs lines of `new` with equal lines of `old`, in order, in one pass:
/// each new line takes the first equal old line at or after the last pair,
/// if one is within [`PAIR_WINDOW`] lines. Linear in the number of lines,
/// whatever the two versions hold.
fn paired_runs(old: &[&str], new: &[&str]) -> Vec<(usize, usize, usize)> {
    let mut places: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (index, line) in old.iter().enumerate() {
        places.entry(line).or_default().push(index);
    }
    let mut runs: Vec<(usize, usize, usize)> = Vec::new();
    let mut next = 0;
    for (in_new, line) in new.iter().enumerate() {
        let Some(found) = places.get(line) else {
            continue;
        };
        let at = found.partition_point(|&place| place < next);
        let Some(&in_old) = found.get(at) else {
            continue;
        };
        if in_old - next > PAIR_WINDOW {
            continue;
        }
        next = in_old + 1;
        match runs.last_mut() {
            Some((run_old, run_new, len))
                if *run_old + *len == in_old && *run_new + *len == in_new =>
            {
                *len += 1;
            }
            _ => runs.push((in_old, in_new, 1)),
        }
    }
    runs
}

fn bounded_ops(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    let shortest = old.len().min(new.len());
    let head = old
        .iter()
        .zip(new)
        .take_while(|(first, second)| first == second)
        .count();
    let tail = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(shortest - head)
        .take_while(|(first, second)| first == second)
        .count();
    let old_middle = &old[head..old.len() - tail];
    let new_middle = &new[head..new.len() - tail];

    let mut ops = Vec::new();
    if head > 0 {
        ops.push(DiffOp::Equal {
            old_index: 0,
            new_index: 0,
            len: head,
        });
    }
    match (old_middle.len(), new_middle.len()) {
        (0, 0) => {}
        (old_len, 0) => ops.push(DiffOp::Delete {
            old_index: head,
            old_len,
            new_index: head,
        }),
        (0, new_len) => ops.push(DiffOp::Insert {
            old_index: head,
            new_index: head,
            new_len,
        }),
        (old_len, new_len) if old_len + new_len > MIDDLE_MAX_LINES => ops.push(DiffOp::Replace {
            old_index: head,
            old_len,
            new_index: head,
            new_len,
        }),
        _ => {
            let middle = capture_diff_slices(Algorithm::Myers, old_middle, new_middle);
            ops.extend(middle.into_iter().map(|op| shifted(op, head)));
        }
    }
    if tail > 0 {
        ops.push(DiffOp::Equal {
            old_index: old.len() - tail,
            new_index: new.len() - tail,
            len: tail,
        });
    }
    ops
}

/// An operation on the middle of two versions, as one on the whole of them.
fn shifted(op: DiffOp, by: usize) -> DiffOp {
    match op {
        DiffOp::Equal {
            old_index,
            new_index,
            len,
        } => DiffOp::Equal {
            old_index: old_index + by,
            new_index: new_index + by,
            len,
        },
        DiffOp::Delete {
            old_index,
            old_len,
            new_index,
        } => DiffOp::Delete {
            old_index: old_index + by,
            old_len,
            new_index: new_index + by,
        },
        DiffOp::Insert {
            old_index,
            new_index,
            new_len,
        } => DiffOp::Insert {
            old_index: old_index + by,
            new_index: new_index + by,
            new_len,
        },
        DiffOp::Replace {
            old_index,
            old_len,
            new_index,
            new_len,
        } => DiffOp::Replace {
            old_index: old_index + by,
            old_len,
            new_index: new_index + by,
            new_len,
        },
    }
}

/// The text of a diff, from the operations that make it.
fn render(old_lines: &[&str], new_lines: &[&str], ops: Vec<DiffOp>) -> Diff {
    let hunks = group_diff_ops(ops.clone(), CONTEXT);

    let mut text = String::new();
    let mut added = 0;
    let mut removed = 0;
    for hunk in &hunks {
        let (Some(first), Some(last)) = (hunk.first(), hunk.last()) else {
            continue;
        };
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        if !text.is_empty() {
            text.push('\n');
        }
        // Writing to a string cannot fail.
        let _ = write!(
            text,
            "@@ -{} +{} @@",
            hunk_range(old_range.start, old_range.len()),
            hunk_range(new_range.start, new_range.len())
        );
        for op in hunk {
            match *op {
                DiffOp::Equal { old_index, len, .. } => {
                    push_lines(&mut text, ' ', &old_lines[old_index..old_index + len]);
                }
                DiffOp::Delete {
                    old_index, old_len, ..
                } => {
                    push_lines(&mut text, '-', &old_lines[old_index..old_index + old_len]);
                    removed += old_len;
                }
                DiffOp::Insert {
                    new_index, new_len, ..
                } => {
                    push_lines(&mut text, '+', &new_lines[new_index..new_index + new_len]);
                    added += new_len;
                }
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    push_lines(&mut text, '-', &old_lines[old_index..old_index + old_len]);
                    push_lines(&mut text, '+', &new_lines[new_index..new_index + new_len]);
                    removed += old_len;
                    added += new_len;
                }
            }
        }
    }
    Diff {
        text,
        added,
        removed,
        ops,
        hunks,
    }
}

/// A range in a hunk header, counted from 1: `12,3`, or `12` for one line,
/// or `11,0` for none, after line 11.
fn hunk_range(start: usize, len: usize) -> String {
    match len {
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{len}", start + 1),
    }
}

fn push_lines(text: &mut String, sign: char, lines: &[&str]) {
    for line in lines {
        text.push('\n');
        text.push(sign);
        text.push_str(line);
    }
}

impl Diff {
    /// Whether the two versions have the same lines.
    pub(crate) fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// The lines of the new version that a model has seen, once it has been
    /// shown this diff and had earlier seen `seen` of the old version:
    ///
    /// - every line it had seen that is still there, at its new number;
    /// - every line that was added, because the diff shows it;
    /// - the unchanged lines the diff shows around each change.
    pub(crate) fn seen_after(&self, seen: &LineRanges) -> LineRanges {
        let mut after = LineRanges::new();
        for op in &self.ops {
            match *op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    // Lines `old_index + 1 ..= old_index + len` of the old
                    // version are lines `new_index + 1 ..` of the new.
                    let first = old_index as u64 + 1;
                    let last = (old_index + len) as u64;
                    for (start, end) in seen.iter() {
                        let (start, end) = (start.max(first), end.min(last));
                        if start <= end {
                            let moved = |line: u64| line - first + new_index as u64 + 1;
                            after.insert(moved(start), moved(end));
                        }
                    }
                }
                DiffOp::Insert {
                    new_index, new_len, ..
                }
                | DiffOp::Replace {
                    new_index, new_len, ..
                } => after.insert(new_index as u64 + 1, (new_index + new_len) as u64),
                DiffOp::Delete { .. } => {}
            }
        }
        for op in self.hunks.iter().flatten() {
            if let DiffOp::Equal { new_index, len, .. } = *op {
                after.insert(new_index as u64 + 1, (new_index + len) as u64);
            }
        }
        after
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;

    use super::*;
    use crate::cases::{self, ensure};

    fn numbered(count: usize) -> String {
        (1..=count).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn a_change_is_shown_with_the_lines_around_it() {
        let old = numbered(10);
        let new = old.replace("line 5\n", "line five\n");
        let diff = diff(&old, &new).expect("small enough");
        assert_eq!(
            diff.text,
            "@@ -2,7 +2,7 @@\n line 2\n line 3\n line 4\n-line 5\n+line five\n line 6\n line 7\n line 8"
        );
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert!(!diff.is_empty());
    }

    #[test]
    fn changes_far_apart_are_separate_hunks() {
        let old = numbered(30);
        let new = old
            .replace("line 3\n", "")
            .replace("line 25\n", "line 25\nextra\n");
        let diff = diff(&old, &new).expect("small enough");
        assert_eq!(
            diff.text,
            "@@ -1,6 +1,5 @@\n line 1\n line 2\n-line 3\n line 4\n line 5\n line 6\n\
             @@ -23,6 +22,7 @@\n line 23\n line 24\n line 25\n+extra\n line 26\n line 27\n line 28"
        );
        assert_eq!((diff.added, diff.removed), (1, 1));
    }

    #[test]
    fn additions_at_the_ends_and_to_an_empty_file_have_the_right_headers() {
        let appended = diff("a\nb\n", "a\nb\nc\n").expect("small");
        assert_eq!(appended.text, "@@ -1,2 +1,3 @@\n a\n b\n+c");
        let created = diff("", "a\nb\n").expect("small");
        assert_eq!(created.text, "@@ -0,0 +1,2 @@\n+a\n+b");
        let emptied = diff("a\n", "").expect("small");
        assert_eq!(emptied.text, "@@ -1 +0,0 @@\n-a");
    }

    #[test]
    fn a_change_of_line_endings_alone_is_no_change_of_lines() {
        let unix = "one\ntwo\n";
        for other in [
            "one\r\ntwo\r\n",
            "one\ntwo",
            "one\ntwo\n",
            "\u{feff}one\ntwo\n",
        ] {
            let diff = diff(unix, other).expect("small");
            assert!(diff.is_empty(), "{other:?}");
            assert_eq!(diff.text, "");
        }
        assert_eq!(lines_of("a\r\nb\r\n"), ["a", "b"]);
        assert_eq!(lines_of("a\n\n"), ["a", ""]);
        assert_eq!(lines_of("\n"), [""]);
        assert!(lines_of("").is_empty());
    }

    #[test]
    fn a_bounded_diff_is_the_same_diff_for_a_change_in_the_middle() {
        let old = numbered(30);
        let new = old
            .replace("line 3\n", "")
            .replace("line 25\n", "line 25\nextra\n");
        let whole = diff(&old, &new).expect("small");
        let bounded = diff_bounded(&old, &new).expect("small");
        assert_eq!(bounded.text, whole.text);
        assert_eq!(
            (bounded.added, bounded.removed),
            (whole.added, whole.removed)
        );
        // Nothing in common, one side empty, and nothing changed.
        assert_eq!(
            diff_bounded("a\nb\n", "c\n").expect("small").text,
            diff("a\nb\n", "c\n").expect("small").text
        );
        assert_eq!(
            diff_bounded("", "a\nb\n").expect("small").text,
            "@@ -0,0 +1,2 @@\n+a\n+b"
        );
        assert_eq!(
            diff_bounded("a\nb\n", "a\n").expect("small").text,
            "@@ -1,2 +1 @@\n a\n-b"
        );
        assert!(diff_bounded("a\nb\n", "a\r\nb").expect("small").is_empty());
        let old = "x\n".repeat(MAX_LINES / 2);
        let new = "y\n".repeat(MAX_LINES / 2 + 1);
        assert!(diff_bounded(&old, &new).is_none());
    }

    #[test]
    fn a_middle_too_large_to_compare_is_shown_as_replaced() {
        // Two versions with the same first and last line and nothing else
        // in common, each longer than the bound.
        let middle = |word: &str| -> String {
            (0..MIDDLE_MAX_LINES)
                .map(|n| format!("{word} {n}\n"))
                .collect()
        };
        let old = format!("first\n{}last\n", middle("old"));
        let new = format!("first\n{}last\n", middle("new"));
        let diff = diff_bounded(&old, &new).expect("within the line limit");
        assert_eq!(
            (diff.added, diff.removed),
            (MIDDLE_MAX_LINES, MIDDLE_MAX_LINES)
        );
        assert!(diff.text.starts_with(&format!(
            "@@ -1,{0} +1,{0} @@\n first\n-old 0\n",
            MIDDLE_MAX_LINES + 2
        )));
        let old_lines = lines_of(&old);
        let new_lines = lines_of(&new);
        assert_eq!(
            common_runs(&old_lines, &new_lines),
            [(0, 0, 1), (MIDDLE_MAX_LINES + 1, MIDDLE_MAX_LINES + 1, 1)]
        );
    }

    #[test]
    fn lines_in_a_middle_too_large_to_compare_are_still_paired() {
        // A long file of which a `write` changes the second line and the
        // last but one, and takes one line out: too much lies between the
        // two changes to compare in full, and every line there is still
        // found to be the line it was.
        let count = MIDDLE_MAX_LINES;
        let old: Vec<String> = (0..count).map(|n| format!("line {n}")).collect();
        let mut new = old.clone();
        new[1] = "changed near the start".to_owned();
        new[count - 2] = "changed near the end".to_owned();
        new.remove(count / 2);
        let old: Vec<&str> = old.iter().map(String::as_str).collect();
        let new: Vec<&str> = new.iter().map(String::as_str).collect();
        let runs = common_runs(&old, &new);
        let covered: usize = runs.iter().map(|run| run.2).sum();
        assert_eq!(covered, count - 3, "every line but the three: {runs:?}");
        for &(in_old, in_new, len) in &runs {
            assert_eq!(old[in_old..in_old + len], new[in_new..in_new + len]);
        }
        // A line that is everywhere does not pair far ahead and strand the
        // lines between.
        let mut far: Vec<String> = (0..PAIR_WINDOW + 10).map(|n| format!("l{n}")).collect();
        far.push("}".to_owned());
        let far: Vec<&str> = far.iter().map(String::as_str).collect();
        let mut new = vec!["}"];
        new.extend(&far[..5]);
        assert_eq!(
            paired_runs(&far, &new),
            [(0, 1, 5)],
            "the `}}` out of reach is passed over, and the lines after it pair"
        );
    }

    #[test]
    fn common_runs_cover_exactly_the_lines_two_versions_share() {
        cases::check(
            "diff_common_runs",
            300,
            |rng| {
                let old: Vec<String> = (0..rng.below(30)).map(|n| format!("l{n}")).collect();
                let mut new = old.clone();
                for _ in 0..rng.below(5) {
                    let at = rng.below(new.len() as u64 + 1) as usize;
                    match rng.below(3) {
                        0 => new.insert(at, format!("added{}", rng.below(1000))),
                        1 if at < new.len() => {
                            new.remove(at);
                        }
                        _ if at < new.len() => new[at] = format!("changed{}", rng.below(1000)),
                        _ => {}
                    }
                }
                (old, new)
            },
            |(old, new)| {
                let old: Vec<&str> = old.iter().map(String::as_str).collect();
                let new: Vec<&str> = new.iter().map(String::as_str).collect();
                let runs = common_runs(&old, &new);
                let (mut old_at, mut new_at) = (0, 0);
                for &(old_index, new_index, len) in &runs {
                    ensure!(
                        old_index >= old_at && new_index >= new_at && len > 0,
                        "runs out of order: {runs:?}"
                    );
                    ensure!(
                        old[old_index..old_index + len] == new[new_index..new_index + len],
                        "a run of lines that differ: {runs:?}"
                    );
                    old_at = old_index + len;
                    new_at = new_index + len;
                }
                // Every line of `old` is distinct, so a line that is still
                // in `new` and has not moved past another must be in a run.
                let kept = new.iter().filter(|line| old.contains(line)).count();
                let covered: usize = runs.iter().map(|run| run.2).sum();
                ensure!(
                    covered == kept,
                    "{covered} of {kept} shared lines: {runs:?}"
                );
                Ok(())
            },
        );
    }

    #[test]
    fn versions_too_large_to_diff_are_not_diffed() {
        let old = "x\n".repeat(MAX_LINES / 2);
        let new = "y\n".repeat(MAX_LINES / 2 + 1);
        assert!(diff(&old, &new).is_none());
    }

    #[test]
    fn seen_lines_move_with_the_text_and_added_lines_count_as_seen() {
        // Twenty lines, of which the model saw 5 to 10. Two lines are added
        // after line 2, and line 8 is rewritten.
        let old = numbered(20);
        let new = old
            .replace("line 2\n", "line 2\nnew a\nnew b\n")
            .replace("line 8\n", "line eight\n");
        let diff = diff(&old, &new).expect("small");
        let after = diff.seen_after(&LineRanges::of(5, 10));
        // Old 5-10 are new 7-12. The added lines are 3-4, and the diff
        // shows the lines around both changes: 1-2 and 5-6, and 7-9, 11-13.
        assert_eq!(after.to_string(), "1-13");

        // With nothing seen before, only what the diff shows is seen: here
        // two hunks far apart, and nothing of the lines between them.
        let old = numbered(30);
        let new = old
            .replace("line 3\n", "")
            .replace("line 25\n", "line 25\nextra\n");
        let diff = super::diff(&old, &new).expect("small");
        assert_eq!(
            diff.seen_after(&LineRanges::new()).to_string(),
            "1-5, 22-28"
        );
        // Lines seen before, between the hunks, are still seen, one line up.
        assert_eq!(
            diff.seen_after(&LineRanges::of(10, 12)).to_string(),
            "1-5, 9-11, 22-28"
        );
    }

    #[test]
    fn a_seen_line_is_one_the_model_was_really_shown() {
        // The property that makes "unchanged, you have seen it" safe to say:
        // every line counted as seen is a line the model saw before, or a
        // line the diff shows.
        cases::check(
            "diff_seen_after",
            300,
            |rng| {
                let mut old: Vec<String> = (0..rng.below(40)).map(|n| format!("l{n}")).collect();
                let mut new = old.clone();
                for _ in 0..rng.below(6) {
                    let at = rng.below(new.len() as u64 + 1) as usize;
                    match rng.below(3) {
                        0 => new.insert(at, format!("added{}", rng.below(1000))),
                        1 if at < new.len() => {
                            new.remove(at);
                        }
                        _ if at < new.len() => new[at] = format!("changed{}", rng.below(1000)),
                        _ => {}
                    }
                }
                // Sometimes a repeated line, which a diff may match either way.
                if rng.below(4) == 0 && !old.is_empty() {
                    let copy = old[0].clone();
                    old.push(copy.clone());
                    new.push(copy);
                }
                let first = rng.below(old.len() as u64 + 1) + 1;
                let last = first + rng.below(15);
                (old, new, first, last)
            },
            |(old, new, first, last)| {
                let old_text: String = old.iter().map(|line| format!("{line}\n")).collect();
                let new_text: String = new.iter().map(|line| format!("{line}\n")).collect();
                let Some(diff) = diff(&old_text, &new_text) else {
                    return Err("small versions were not diffed".into());
                };
                let seen = LineRanges::of(*first, (*last).min(old.len() as u64));
                let after = diff.seen_after(&seen);
                let shown: Vec<&str> = diff
                    .text
                    .lines()
                    .filter(|line| line.starts_with(' ') || line.starts_with('+'))
                    .map(|line| &line[1..])
                    .collect();
                for (start, end) in after.iter() {
                    ensure!(
                        end <= new.len() as u64,
                        "line {end} is past the end of the new version"
                    );
                    for number in start..=end {
                        let line = new[number as usize - 1].as_str();
                        let seen_before = seen
                            .iter()
                            .any(|(a, b)| (a..=b).any(|n| old[n as usize - 1] == line));
                        ensure!(
                            seen_before || shown.contains(&line),
                            "line {number} ({line:?}) counts as seen and was never shown"
                        );
                    }
                }
                // And the whole file counts as seen when all of it was.
                let all = diff.seen_after(&LineRanges::of(1, old.len() as u64));
                ensure!(
                    all.covers(1, new.len() as u64),
                    "all of the old version was seen, and {all} of the new counts as seen"
                );
                Ok(())
            },
        );
    }
}
