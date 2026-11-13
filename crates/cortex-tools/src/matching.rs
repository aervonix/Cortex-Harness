//! Finding the text an `edit` call names, in two tiers
//! (`docs/architecture/05-tools.md`, "`edit`";
//! `docs/decisions/0020-edit-matching.md`).
//!
//! 1. **Exact.** `old` is looked for in the file's text as `read` shows it:
//!    with every line break a line feed, whatever the file's line endings
//!    are. An `old` copied from a `read` of a CRLF file is therefore an
//!    exact match.
//! 2. **Tolerant**, only when the exact tier finds nothing: `old` is
//!    matched line by line against whole lines of the file, ignoring
//!    trailing spaces, tabs and carriage returns, and a uniform difference
//!    in leading indentation (the file's lines all have the same extra
//!    indentation in front, or `old`'s do). It is accepted only when it
//!    finds exactly `count` places, and it is always reported.
//!
//! There is nothing further: no Unicode folding, no edit distance. A wrong
//! edit is worse than a failed one.
//!
//! Everything here is a pure function of the file's text and the edits:
//! every `old` is matched against the file as it was before the call, the
//! places found must not overlap, and either every edit has its places or
//! nothing is returned to apply.
//!
//! **The work is bounded and can be stopped.** A file is up to 16 MiB and a
//! call up to a hundred edits, so the search asks, between edits and every
//! few thousand lines, whether the call has been cancelled; the tolerant
//! tier looks at one line of each candidate place before it compares the
//! rest; and the closest text is worked out for the first few edits that
//! fail, not for a hundred.

use std::collections::HashMap;
use std::ops::Range;

use crate::diff;
use crate::textfile::{Text, with_ending};

/// One replacement the model asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edit {
    pub(crate) old: String,
    pub(crate) new: String,
    /// How many times `old` must occur.
    pub(crate) count: usize,
}

/// One place where text is replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replacement {
    /// The bytes of the file's text that go.
    pub(crate) range: Range<usize>,
    /// What is written in their place, with the file's line endings.
    pub(crate) text: String,
    /// Which edit of the call this is for, counted from 0.
    pub(crate) edit: usize,
    /// The lines of the file, counted from 1, that the replaced text is on.
    pub(crate) lines: (usize, usize),
    /// Set when the tolerant tier found the place.
    pub(crate) tolerant: Option<Tolerant>,
}

/// What the tolerant tier did, for the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tolerant {
    /// The text of the file that was replaced, as `read` shows it.
    pub(crate) replaced: String,
    /// How the indentation of `new` was changed to fit, in words. Empty
    /// when it was not.
    pub(crate) reindented: String,
}

/// Why an edit has no place, or the edits cannot be applied together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Neither tier found `old`.
    NotFound {
        edit: usize,
        /// The part of the file most like `old`, if any part is.
        closest: Option<Closest>,
    },
    /// `old` occurs, and not `count` times.
    WrongCount {
        edit: usize,
        wanted: usize,
        /// The first line of each occurrence found, counted from 1.
        at: Vec<usize>,
        /// Whether more occurrences exist than `at` lists.
        more: bool,
        /// Whether the occurrences were found by the tolerant tier.
        tolerant: bool,
    },
    /// Two occurrences of one `old` share text, so "replace both" has no
    /// one meaning.
    SelfOverlap { edit: usize, at: Vec<usize> },
    /// Two edits would replace the same text.
    Overlap {
        first: usize,
        second: usize,
        first_lines: (usize, usize),
        second_lines: (usize, usize),
    },
    /// The tolerant tier found the place, and `new` cannot be given the
    /// same change of indentation: one of its lines has less than `old`'s
    /// extra.
    CannotReindent {
        edit: usize,
        lines: (usize, usize),
        extra: String,
    },
    /// The call was cancelled while the text was being looked for.
    Stopped,
}

impl Failure {
    /// The edit the failure is about (the first of two).
    pub(crate) fn edit(&self) -> usize {
        match self {
            Self::NotFound { edit, .. }
            | Self::WrongCount { edit, .. }
            | Self::SelfOverlap { edit, .. }
            | Self::CannotReindent { edit, .. } => *edit,
            Self::Overlap { first, .. } => *first,
            Self::Stopped => usize::MAX,
        }
    }
}

/// The part of a file most like an `old` that was not found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Closest {
    /// Its lines, counted from 1.
    pub(crate) lines: (usize, usize),
    /// How it differs from `old`, as a diff from `old` to the file. Empty
    /// when the lines are the same and the difference is elsewhere.
    pub(crate) diff: String,
    /// A difference that a diff does not show: spaces against tabs.
    pub(crate) note: Option<String>,
}

/// How many occurrences past `count` are looked for before the search
/// stops: enough to say where they are.
const EXTRA_OCCURRENCES: usize = 20;

/// The longest `old`, in lines, whose closest region is shown.
const CLOSEST_MAX_LINES: usize = 40;

/// How much text, in bytes, a run of the file must share with `old`, as
/// whole lines, to be called the closest. Under it, the file's lines are
/// compared with `old`'s longest line instead.
const CLOSEST_MIN_SHARED: usize = 4;

/// For how many failed edits of one call the closest text is worked out.
/// Each costs a pass over the file.
const CLOSEST_MAX_EDITS: usize = 5;

/// How many candidate places the tolerant tier looks at between two looks
/// at whether the call has been cancelled.
const STOP_CHECK_EVERY: usize = 4096;

/// Of one line's text in the file, how many places are remembered when
/// looking for the closest region. A line such as `}` is everywhere.
const CLOSEST_MAX_PLACES: usize = 64;

/// Finds the place of every edit. `Ok` holds the replacements in the order
/// of the file, none overlapping another. `Err` holds everything that was
/// wrong, so that one new call can put all of it right.
///
/// `stop` is asked now and then whether to give up: when it says yes, the
/// one failure returned is [`Failure::Stopped`].
pub(crate) fn locate(
    text: &Text<'_>,
    edits: &[Edit],
    stop: &dyn Fn() -> bool,
) -> Result<Vec<Replacement>, Vec<Failure>> {
    let view = text.view();
    let mut found = Vec::new();
    let mut failures = Vec::new();
    let mut not_found = 0;
    for (index, edit) in edits.iter().enumerate() {
        if stop() {
            return Err(vec![Failure::Stopped]);
        }
        let with_closest = not_found < CLOSEST_MAX_EDITS;
        match locate_one(text, &view, index, edit, with_closest, stop) {
            Ok(places) => found.extend(places),
            Err(Failure::Stopped) => return Err(vec![Failure::Stopped]),
            Err(failure) => {
                not_found += usize::from(matches!(failure, Failure::NotFound { .. }));
                failures.push(failure);
            }
        }
    }
    found.sort_by_key(|place: &Replacement| (place.range.start, place.range.end));
    for pair in found.windows(2) {
        let (first, second) = (&pair[0], &pair[1]);
        if second.range.start < first.range.end {
            failures.push(Failure::Overlap {
                first: first.edit.min(second.edit),
                second: first.edit.max(second.edit),
                first_lines: if first.edit <= second.edit {
                    first.lines
                } else {
                    second.lines
                },
                second_lines: if first.edit <= second.edit {
                    second.lines
                } else {
                    first.lines
                },
            });
        }
    }
    if failures.is_empty() {
        Ok(found)
    } else {
        failures.sort_by_key(Failure::edit);
        failures.dedup();
        Err(failures)
    }
}

/// The text with every replacement made. `replacements` are in the order
/// of the file and do not overlap, as [`locate`] returns them.
pub(crate) fn apply(text: &str, replacements: &[Replacement]) -> String {
    let grown: usize = replacements.iter().map(|place| place.text.len()).sum();
    let mut result = String::with_capacity(text.len() + grown);
    let mut at = 0;
    for place in replacements {
        result.push_str(&text[at..place.range.start]);
        result.push_str(&place.text);
        at = place.range.end;
    }
    result.push_str(&text[at..]);
    result
}

/// Where each replacement is in the text [`apply`] made: the lines, counted
/// from 1, that hold what was written. A replacement that wrote nothing is
/// at the line where the text was taken out, and has `None` as its last.
pub(crate) fn written_lines(
    text: &str,
    replacements: &[Replacement],
) -> Vec<(usize, Option<usize>)> {
    let mut lines = Vec::with_capacity(replacements.len());
    let mut at = 0;
    // Line breaks in the result so far.
    let mut breaks = 0;
    for place in replacements {
        breaks += count_breaks(&text[at..place.range.start]);
        at = place.range.end;
        let first = breaks + 1;
        let inside = count_breaks(&place.text);
        let last = if place.text.is_empty() {
            None
        } else if place.text.ends_with('\n') {
            // The break ends the last line written; it does not start one.
            Some(first + inside - 1)
        } else {
            Some(first + inside)
        };
        lines.push((first, last));
        breaks += inside;
    }
    lines
}

fn count_breaks(text: &str) -> usize {
    text.bytes().filter(|&byte| byte == b'\n').count()
}

fn locate_one(
    text: &Text<'_>,
    view: &crate::textfile::View<'_>,
    index: usize,
    edit: &Edit,
    with_closest: bool,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<Replacement>, Failure> {
    // Enough places to say where the extra ones are, and no more. The
    // count is the model's: it may be any number.
    let limit = edit.count.saturating_add(EXTRA_OCCURRENCES);
    // Tier 1.
    let (starts, more) = occurrences(&view.text, &edit.old, limit);
    if !starts.is_empty() {
        let ranges: Vec<Range<usize>> = starts
            .iter()
            .map(|&start| view.to_file(start)..view.to_file(start + edit.old.len()))
            .collect();
        let at = |ranges: &[Range<usize>]| {
            ranges
                .iter()
                .map(|range| text.line_at(range.start) + 1)
                .collect()
        };
        if more || ranges.len() != edit.count {
            return Err(Failure::WrongCount {
                edit: index,
                wanted: edit.count,
                at: at(&ranges),
                more,
                tolerant: false,
            });
        }
        if ranges.windows(2).any(|pair| pair[1].start < pair[0].end) {
            return Err(Failure::SelfOverlap {
                edit: index,
                at: at(&ranges),
            });
        }
        return Ok(ranges
            .into_iter()
            .map(|range| Replacement {
                text: with_ending(&edit.new, text.ending_at(range.start)),
                lines: (
                    text.line_at(range.start) + 1,
                    text.line_at(range.end.saturating_sub(1).max(range.start)) + 1,
                ),
                range,
                edit: index,
                tolerant: None,
            })
            .collect());
    }

    // Tier 2.
    let old_lines = tolerant_lines(&edit.old);
    let Some(windows) = tolerant_windows(text, &old_lines, limit, stop) else {
        return Err(Failure::Stopped);
    };
    if windows.is_empty() {
        return Err(Failure::NotFound {
            edit: index,
            closest: if with_closest {
                closest(text, &edit.old)
            } else {
                None
            },
        });
    }
    let at: Vec<usize> = windows.iter().map(|(start, _)| start + 1).collect();
    if windows.len() != edit.count {
        return Err(Failure::WrongCount {
            edit: index,
            wanted: edit.count,
            more: windows.len() == limit,
            at,
            tolerant: true,
        });
    }
    if windows
        .windows(2)
        .any(|pair| pair[1].0 < pair[0].0 + old_lines.len())
    {
        return Err(Failure::SelfOverlap { edit: index, at });
    }
    windows
        .into_iter()
        .map(|(start, shift)| {
            tolerant_replacement(text, index, edit, start, old_lines.len(), &shift)
        })
        .collect()
}

/// Every place `needle` begins in `text`, places that share text included,
/// up to `limit` of them, and whether there are more.
fn occurrences(text: &str, needle: &str, limit: usize) -> (Vec<usize>, bool) {
    let mut starts = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(needle) {
        let start = from + found;
        if starts.len() == limit {
            return (starts, true);
        }
        starts.push(start);
        // The next place may begin inside this one.
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    (starts, false)
}

/// The spaces, tabs and carriage returns at the end of a line are not
/// compared by the tolerant tier.
fn trim_end(line: &str) -> &str {
    line.trim_end_matches([' ', '\t', '\r'])
}

/// The indentation of a line: its leading spaces and tabs.
fn indent(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// The lines of an `old` or a `new` for the tolerant tier: split at line
/// feeds, where a final line break ends the last line. An empty text has no
/// lines.
fn tolerant_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// A uniform difference in leading indentation between the file's lines and
/// `old`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shift {
    None,
    /// Every line of the file has this in front of what `old` has.
    FileDeeper(String),
    /// Every line of `old` has this in front of what the file has.
    OldDeeper(String),
}

impl Shift {
    /// The shift between one line of the file and one of `old`, both
    /// without trailing white space and neither blank. `None` when they are
    /// different lines.
    fn between(file: &str, old: &str) -> Option<Self> {
        if file == old {
            return Some(Self::None);
        }
        let (file_indent, old_indent) = (indent(file), indent(old));
        if file[file_indent.len()..] != old[old_indent.len()..] {
            return None;
        }
        if let Some(extra) = file_indent.strip_suffix(old_indent) {
            Some(Self::FileDeeper(extra.to_owned()))
        } else {
            // Spaces against tabs, say: not a uniform difference.
            old_indent
                .strip_suffix(file_indent)
                .map(|extra| Self::OldDeeper(extra.to_owned()))
        }
    }

    fn holds(&self, file: &str, old: &str) -> bool {
        match self {
            Self::None => file == old,
            Self::FileDeeper(extra) => file.strip_prefix(extra.as_str()) == Some(old),
            Self::OldDeeper(extra) => old.strip_prefix(extra.as_str()) == Some(file),
        }
    }
}

/// Every run of whole lines of the file that `old_lines` matches with the
/// tolerant tier's rules: its first line, and the shift. Up to `limit`.
/// `None` when `stop` said to give up.
fn tolerant_windows(
    text: &Text<'_>,
    old_lines: &[&str],
    limit: usize,
    stop: &dyn Fn() -> bool,
) -> Option<Vec<(usize, Shift)>> {
    let old: Vec<&str> = old_lines.iter().map(|line| trim_end(line)).collect();
    // An `old` of blank lines names nothing: every gap in the file is one.
    let Some(anchor) = old.iter().position(|line| !line.is_empty()) else {
        return Some(Vec::new());
    };
    if old.len() > text.lines.len() {
        return Some(Vec::new());
    }
    let mut windows = Vec::new();
    'starts: for start in 0..=text.lines.len() - old.len() {
        if start % STOP_CHECK_EVERY == STOP_CHECK_EVERY - 1 && stop() {
            return None;
        }
        // One line decides for nearly every place: the first line of `old`
        // that is not blank, against the line it would lie on. Without
        // this, an `old` that begins with blank lines would be compared in
        // full at every gap of a file full of them.
        let on = trim_end(text.content(start + anchor));
        if on.is_empty() || Shift::between(on, old[anchor]).is_none() {
            continue;
        }
        let mut shift: Option<Shift> = None;
        for (offset, old_line) in old.iter().enumerate() {
            let file_line = trim_end(text.content(start + offset));
            if old_line.is_empty() || file_line.is_empty() {
                if old_line.is_empty() && file_line.is_empty() {
                    continue;
                }
                continue 'starts;
            }
            match &shift {
                Some(shift) if shift.holds(file_line, old_line) => {}
                Some(_) => continue 'starts,
                None => match Shift::between(file_line, old_line) {
                    Some(found) => shift = Some(found),
                    None => continue 'starts,
                },
            }
        }
        if let Some(shift) = shift {
            windows.push((start, shift));
            if windows.len() == limit {
                break;
            }
        }
    }
    Some(windows)
}

/// The lines of a new text as they are written: split at line feeds,
/// without the carriage return of a line break the model wrote as CRLF. A
/// carriage return at the very end of a text, with no line feed after it,
/// is not part of a line break: it is content, and stays.
fn new_lines(text: &str) -> Vec<&str> {
    let lines = tolerant_lines(text);
    let last = lines.len().saturating_sub(1);
    let ends_with_break = text.ends_with('\n');
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index < last || ends_with_break {
                line.strip_suffix('\r').unwrap_or(line)
            } else {
                line
            }
        })
        .collect()
}

fn tolerant_replacement(
    text: &Text<'_>,
    index: usize,
    edit: &Edit,
    start: usize,
    count: usize,
    shift: &Shift,
) -> Result<Replacement, Failure> {
    let first = text.lines[start];
    let last = text.lines[start + count - 1];
    let lines = (start + 1, start + count);
    // Whole lines are replaced by whole lines: every line written but the
    // last ends as the first line replaced did, and the last ends as the
    // last line replaced did, which at the end of a file may be not at all.
    let between = first.ending.unwrap_or(text.dominant);
    let new_lines = new_lines(&edit.new);
    let mut written = String::with_capacity(edit.new.len() + 16);
    for (position, line) in new_lines.iter().enumerate() {
        let blank = trim_end(line).is_empty();
        match shift {
            Shift::FileDeeper(extra) if !blank => {
                written.push_str(extra);
                written.push_str(line);
            }
            Shift::OldDeeper(extra) => match line.strip_prefix(extra.as_str()) {
                Some(rest) => written.push_str(rest),
                None if blank => written.push_str(line),
                None => {
                    return Err(Failure::CannotReindent {
                        edit: index,
                        lines,
                        extra: describe_indent(extra),
                    });
                }
            },
            _ => written.push_str(line),
        }
        if position + 1 < new_lines.len() {
            written.push_str(between.as_str());
        } else if let Some(ending) = last.ending {
            written.push_str(ending.as_str());
        }
    }
    let reindented = match shift {
        Shift::None => String::new(),
        Shift::FileDeeper(extra) => format!("{} added to", describe_indent(extra)),
        Shift::OldDeeper(extra) => format!("{} taken off", describe_indent(extra)),
    };
    Ok(Replacement {
        range: first.start..last.after(),
        text: written,
        edit: index,
        lines,
        tolerant: Some(Tolerant {
            replaced: (start..start + count)
                .map(|line| text.content(line))
                .collect::<Vec<_>>()
                .join("\n"),
            reindented,
        }),
    })
}

/// An indentation in words: `4 spaces`, `1 tab`, `2 tabs and 2 spaces`.
pub(crate) fn describe_indent(indent: &str) -> String {
    let tabs = indent.matches('\t').count();
    let spaces = indent.matches(' ').count();
    let plural = |count: usize, word: &str| {
        if count == 1 {
            format!("1 {word}")
        } else {
            format!("{count} {word}s")
        }
    };
    match (tabs, spaces) {
        (0, 0) => "no indentation".to_owned(),
        (tabs, 0) => plural(tabs, "tab"),
        (0, spaces) => plural(spaces, "space"),
        (tabs, spaces) => format!("{} and {}", plural(tabs, "tab"), plural(spaces, "space")),
    }
}

/// The part of the file most like `old`: the run of lines that shares the
/// most lines with it, whatever their indentation, or failing that the run
/// around the line most like `old`'s longest.
fn closest(text: &Text<'_>, old: &str) -> Option<Closest> {
    let old_lines = tolerant_lines(old);
    if old_lines.is_empty() || old_lines.len() > CLOSEST_MAX_LINES || text.lines.is_empty() {
        return None;
    }
    let trimmed: Vec<&str> = old_lines.iter().map(|line| line.trim()).collect();

    // Where each line of the file is, by its text without indentation.
    let wanted: std::collections::HashSet<&str> = trimmed
        .iter()
        .copied()
        .filter(|line| !line.is_empty())
        .collect();
    let mut places: HashMap<&str, Vec<usize>> = HashMap::new();
    for line in 0..text.lines.len() {
        let content = text.content(line).trim();
        if wanted.contains(content) {
            let found = places.entry(content).or_default();
            if found.len() < CLOSEST_MAX_PLACES {
                found.push(line);
            }
        }
    }
    // Each line of `old` that is in the file votes for the run it would be
    // part of.
    let mut votes: HashMap<usize, usize> = HashMap::new();
    for (offset, line) in trimmed.iter().enumerate() {
        for &place in places.get(line).map_or(&[][..], Vec::as_slice) {
            if let Some(start) = place.checked_sub(offset) {
                // A longer line says more: `}` is everywhere.
                *votes.entry(start).or_default() += line.len();
            }
        }
    }
    let start = votes
        .into_iter()
        .max_by(|first, second| first.1.cmp(&second.1).then(second.0.cmp(&first.0)))
        .filter(|&(_, weight)| weight >= CLOSEST_MIN_SHARED)
        .map(|(start, _)| start)
        .or_else(|| nearest_line(text, &trimmed))?;

    let end = (start + old_lines.len()).min(text.lines.len());
    let region: Vec<&str> = (start..end).map(|line| text.content(line)).collect();
    let shown_old: Vec<&str> = old_lines
        .iter()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let note = region
        .iter()
        .zip(&shown_old)
        .enumerate()
        .find(|(_, (file, old))| {
            trim_end(file) != trim_end(old) && file.trim() == old.trim() && !file.trim().is_empty()
        })
        .map(|(offset, (file, old))| {
            format!(
                "Line {} of the file is indented with {}, and the matching line of `old` with {}.",
                start + offset + 1,
                describe_indent(indent(file)),
                describe_indent(indent(old)),
            )
        });
    let diff = diff::diff(&shown_old.join("\n"), &region.join("\n"))
        .map(|diff| diff.text)
        .unwrap_or_default();
    Some(Closest {
        lines: (start + 1, end),
        diff,
        note,
    })
}

/// Where the run would start whose line is most like `old`'s longest line,
/// by the text the two share at their beginnings and ends. `None` when no
/// line of the file is much like it.
fn nearest_line(text: &Text<'_>, old: &[&str]) -> Option<usize> {
    let (offset, longest) = old
        .iter()
        .enumerate()
        .max_by_key(|(offset, line)| (line.len(), std::cmp::Reverse(*offset)))?;
    if longest.len() < CLOSEST_MIN_SHARED {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    for line in 0..text.lines.len() {
        let content = text.content(line).trim();
        let shorter = content.len().min(longest.len());
        let longer = content.len().max(longest.len());
        // Not worth comparing: too different in length to pass the bar.
        if shorter * 5 < longer * 3 {
            continue;
        }
        let front = content
            .bytes()
            .zip(longest.bytes())
            .take_while(|(first, second)| first == second)
            .count();
        let back = content
            .bytes()
            .rev()
            .zip(longest.bytes().rev())
            .take_while(|(first, second)| first == second)
            .count();
        let shared = (front + back).min(shorter);
        // At least three fifths of the longer line.
        if shared * 5 >= longer * 3 && best.is_none_or(|(_, most)| shared > most) {
            best = Some((line, shared));
        }
    }
    best.and_then(|(line, _)| line.checked_sub(offset).or(Some(0)))
}

/// What a `write` of new content over an existing file writes: `content`'s
/// lines with the file's line endings.
///
/// A line that the old and the new content have in common keeps the ending
/// it had. Any other line gets the file's ending. The last line ends with a
/// line break if `content` does. So a file with CRLF endings stays one, and
/// in a file with both kinds, the lines a write does not change keep
/// theirs. Over a file with no line break at all, the content is written
/// as it was given.
pub(crate) fn rewrite(old: &Text<'_>, content: &str) -> String {
    // A file with no line break has no line ending to keep.
    if old.lines.iter().all(|line| line.ending.is_none()) {
        return content.to_owned();
    }
    let new_lines = new_lines(content);
    // The ending each new line gets. Only a file with both kinds of ending
    // needs to know which of its lines stayed.
    let mut endings = vec![old.dominant; new_lines.len()];
    let mixed = old
        .lines
        .iter()
        .any(|line| line.ending.is_some_and(|ending| ending != old.dominant));
    if mixed {
        let old_lines: Vec<&str> = (0..old.lines.len()).map(|line| old.content(line)).collect();
        for (old_index, new_index, len) in diff::common_runs(&old_lines, &new_lines) {
            for offset in 0..len {
                if let Some(ending) = old.lines[old_index + offset].ending {
                    endings[new_index + offset] = ending;
                }
            }
        }
    }
    let mut written = String::with_capacity(content.len() + new_lines.len());
    let ends_with_break = content.ends_with('\n');
    for (index, line) in new_lines.iter().enumerate() {
        written.push_str(line);
        if index + 1 < new_lines.len() || ends_with_break {
            written.push_str(endings[index].as_str());
        }
    }
    written
}

#[cfg(test)]
mod tests;
