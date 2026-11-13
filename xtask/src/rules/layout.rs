//! The layout of the step records and the open items (`docs/steps/` and
//! `docs/outstanding/`, rule 14 of the build plan).
//!
//! Each step writes its records in a folder of its own, and each open item
//! is a file of its own, so that two steps written at the same time never
//! edit the same lines. That only holds while every step puts its files
//! where the others expect them, under the names and headings they expect:
//!
//! - `docs/steps/<step>/answered.md` and `docs/steps/<step>/ci.md`, and
//!   nothing else in a step's folder. The folder is the step's number in
//!   two digits with its letter if it has one (`07`, `13a`), or, for work
//!   that is not a numbered step, a name in lower-case words joined by
//!   hyphens (`tooling-w5`).
//! - The first line of `answered.md` is `# Answered in step 7, on
//!   2026-11-13`, and of `ci.md` is `# After step 7: the agent loop`, with
//!   the step the folder is named for. A folder with a name has the same
//!   headings with its own words in place of `step 7`.
//! - `docs/outstanding/<item>.md`, named in lower-case words joined by
//!   hyphens, holds the table's header and one row.

use std::collections::BTreeMap;

use super::Violation;

const STEPS: &str = "docs/steps/";
const OUTSTANDING: &str = "docs/outstanding/";

/// Where the layout is described, for the messages.
const STEPS_PAGE: &str = "docs/steps/README.md";
const OUTSTANDING_PAGE: &str = "docs/outstanding/README.md";

/// The header of an open item's table.
const ITEM_HEADER: [&str; 2] = ["| Item | Who | When |", "|---|---|---|"];

/// Whether a file is one this rule looks at: anything under the two
/// directories, whatever its ending.
pub(super) fn is_read(file: &str) -> bool {
    file.starts_with(STEPS) || file.starts_with(OUTSTANDING)
}

/// Checks every file under the two directories. `texts` holds each file's
/// text by its path from the repository's root.
pub(crate) fn check(texts: &BTreeMap<String, String>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (file, text) in texts {
        let problem = if let Some(rest) = file.strip_prefix(STEPS) {
            step_record(rest, text)
        } else if let Some(rest) = file.strip_prefix(OUTSTANDING) {
            open_item(rest, text)
        } else {
            None
        };
        if let Some((line, problem)) = problem {
            violations.push(Violation::new(file, line, problem));
        }
    }
    violations
}

/// Lower-case words of letters and digits, joined by hyphens.
fn is_hyphenated(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// A step's folder name as its headings write the step: `07` is `step 7`,
/// `13a` is `step 13a`. `None` for a folder that is not a numbered step.
fn step_of(folder: &str) -> Option<String> {
    let digits = folder.get(..2)?;
    let letter = folder.get(2..)?;
    let numbered = digits.chars().all(|c| c.is_ascii_digit())
        && letter.len() <= 1
        && letter.chars().all(|c| c.is_ascii_lowercase());
    let number: u32 = digits.parse().ok()?;
    numbered.then(|| format!("step {number}{letter}"))
}

/// Whether `text` is a date as the records write one: `2026-11-13`.
fn is_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    matches!(parts.as_slice(), [year, month, day]
        if year.len() == 4 && month.len() == 2 && day.len() == 2
            && parts.iter().all(|part| part.chars().all(|c| c.is_ascii_digit())))
}

/// What is wrong with a file under `docs/steps/`, given its path from
/// there: the line it is at, if it is at one, and what to do.
fn step_record(path: &str, text: &str) -> Option<(Option<usize>, String)> {
    if path == "README.md" {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let [folder, file] = parts.as_slice() else {
        return Some((
            None,
            format!(
                "a step's records are `{STEPS}<step>/answered.md` and `{STEPS}<step>/ci.md`, \
                 and this is neither.\n    {STEPS_PAGE} has the layout."
            ),
        ));
    };
    let step = step_of(folder);
    // A name must not look like a number that was written wrongly: `7`,
    // `007`, `13A`.
    let named = is_hyphenated(folder) && !folder.starts_with(|c: char| c.is_ascii_digit());
    if step.is_none() && !named {
        return Some((
            None,
            format!(
                "`{folder}` is not a step's folder.\n    A folder is the step's number in two \
                 digits with its letter if it has one (`07`, `13a`), so that the folders sort \
                 in step order; or, for work that is not a numbered step, a name in lower-case \
                 words joined by hyphens. {STEPS_PAGE} has the layout."
            ),
        ));
    }
    let (begins, then) = match *file {
        "answered.md" => ("# Answered in ", ", on "),
        "ci.md" => ("# After ", ": "),
        _ => {
            return Some((
                None,
                format!(
                    "a step's folder holds `answered.md` (what the owner answered) and `ci.md` \
                     (the step's CI times), and nothing else.\n    An item left open is a file \
                     in `{OUTSTANDING}`. {STEPS_PAGE} has the layout."
                ),
            ));
        }
    };

    let heading = text.lines().next().unwrap_or("");
    let expected = match (&step, *file) {
        (Some(step), "answered.md") => format!("# Answered in {step}, on <date>"),
        (Some(step), _) => format!("# After {step}: <what it built>"),
        (None, "answered.md") => "# Answered in <the work's name>, on <date>".to_owned(),
        (None, _) => "# After <the work's name>: <what it built>".to_owned(),
    };
    let well_formed = heading
        .strip_prefix(begins)
        .and_then(|rest| rest.split_once(then))
        .is_some_and(|(who, rest)| {
            let who_fits = match &step {
                Some(step) => who == step,
                None => !who.trim().is_empty(),
            };
            let rest_fits = if *file == "answered.md" {
                is_date(rest)
            } else {
                !rest.trim().is_empty()
            };
            who_fits && rest_fits
        });
    if well_formed {
        return None;
    }
    Some((
        Some(1),
        format!(
            "the first line is `{heading}`, and for this file it is `{expected}`.\n    The \
             heading says which step the record is of, in the form every other record has, so \
             that the records read in order as one page. {STEPS_PAGE} has the layout."
        ),
    ))
}

/// What is wrong with a file under `docs/outstanding/`.
fn open_item(path: &str, text: &str) -> Option<(Option<usize>, String)> {
    if path == "README.md" {
        return None;
    }
    let name = path.strip_suffix(".md");
    if path.contains('/') || !name.is_some_and(is_hyphenated) {
        return Some((
            None,
            format!(
                "an open item is one file in `{OUTSTANDING}`, named for the item in lower-case \
                 words joined by hyphens, ending `.md`.\n    {OUTSTANDING_PAGE} has the layout."
            ),
        ));
    }
    let lines: Vec<&str> = text.lines().collect();
    let content = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(&[][..], |last| &lines[..=last]);
    let row_fits = |row: &str| row.starts_with("| ") && row.trim_end().ends_with('|');
    // The first line that is not what it should be: a line of the header,
    // the row that is missing or is not a row, or a line after the row.
    let wrong_header = content
        .iter()
        .zip(ITEM_HEADER)
        .position(|(found, expected)| *found != expected);
    let line = match (wrong_header, content.get(2)) {
        (Some(index), _) => index + 1,
        (None, None) => content.len() + 1,
        (None, Some(row)) if !row_fits(row) => 3,
        (None, Some(_)) if content.len() > 3 => 4,
        (None, Some(_)) => return None,
    };
    Some((
        Some(line),
        format!(
            "an open item is the table's header, `{}`, the line `{}`, and one row: the item, \
             who settles it, and when.\n    One item to a file: a second item is a second \
             file. {OUTSTANDING_PAGE} has the layout.",
            ITEM_HEADER[0], ITEM_HEADER[1]
        ),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::check;

    fn found(files: &[(&str, &str)]) -> Vec<String> {
        let texts: BTreeMap<String, String> = files
            .iter()
            .map(|(file, text)| ((*file).to_owned(), (*text).to_owned()))
            .collect();
        check(&texts)
            .into_iter()
            .map(|violation| violation.to_string())
            .collect()
    }

    fn assert_one(found: &[String], needles: &[&str]) {
        assert_eq!(found.len(), 1, "{found:#?}");
        for needle in needles {
            assert!(
                found[0].contains(needle),
                "`{needle}` is not in:\n{}",
                found[0]
            );
        }
    }

    const ITEM: &str = "| Item | Who | When |\n|---|---|---|\n| **A flag.** It has a recommendation. | Step 13 | In that step |\n";

    #[test]
    fn records_and_items_in_their_places_pass() {
        assert_eq!(
            found(&[
                ("docs/steps/README.md", "# Step records\n"),
                (
                    "docs/steps/07/answered.md",
                    "# Answered in step 7, on 2026-11-13\n\nText.\n"
                ),
                ("docs/steps/07/ci.md", "# After step 7: the agent loop\n"),
                (
                    "docs/steps/13a/answered.md",
                    "# Answered in step 13a, on 2026-11-14\n"
                ),
                ("docs/steps/13a/ci.md", "# After step 13a: core assembly\n"),
                (
                    "docs/steps/tooling-w5/answered.md",
                    "# Answered in the wave 5 tooling step, on 2026-11-13\n"
                ),
                (
                    "docs/steps/tooling-w5/ci.md",
                    "# After the wave 5 tooling step: developer tooling\n"
                ),
                ("docs/outstanding/README.md", "# Still outstanding\n"),
                ("docs/outstanding/speed-flag-for-headless-mode.md", ITEM),
                ("docs/outstanding/quickjs-2.md", &format!("{ITEM}\n\n")),
                // Nothing here is this rule's.
                ("docs/build-plan.md", "no heading at all\n"),
            ]),
            [] as [&str; 0]
        );
    }

    #[test]
    fn a_folder_that_is_not_a_steps_is_refused() {
        for folder in ["7", "007", "13A", "Step-07", "step_07", "07-loop", "1a"] {
            let file = format!("docs/steps/{folder}/ci.md");
            let found = found(&[(&file, "# After step 7: the agent loop\n")]);
            assert_one(&found, &[&format!("`{folder}` is not a step's folder")]);
        }
    }

    #[test]
    fn a_steps_folder_holds_its_two_records_and_nothing_else() {
        assert_one(
            &found(&[("docs/steps/07/notes.md", "# Notes\n")]),
            &["holds `answered.md`", "`docs/outstanding/`"],
        );
        assert_one(
            &found(&[("docs/steps/07/ci/run-1.md", "# After step 7: x\n")]),
            &["and this is neither"],
        );
        assert_one(
            &found(&[("docs/steps/notes.md", "# Notes\n")]),
            &["and this is neither"],
        );
    }

    #[test]
    fn a_records_heading_names_its_own_step_in_the_form_the_others_have() {
        for (file, heading, expected) in [
            // Another step's heading.
            (
                "docs/steps/07/answered.md",
                "# Answered in step 8, on 2026-11-13",
                "`# Answered in step 7, on <date>`",
            ),
            // The folder's spelling, not the step's.
            (
                "docs/steps/07/answered.md",
                "# Answered in step 07, on 2026-11-13",
                "`# Answered in step 7, on <date>`",
            ),
            (
                "docs/steps/07/answered.md",
                "# Answered in step 7, on 13 November",
                "`# Answered in step 7, on <date>`",
            ),
            (
                "docs/steps/07/answered.md",
                "## Answered in step 7, on 2026-11-13",
                "`# Answered in step 7, on <date>`",
            ),
            (
                "docs/steps/13a/ci.md",
                "# After step 13: core assembly",
                "`# After step 13a: <what it built>`",
            ),
            (
                "docs/steps/07/ci.md",
                "# After step 7",
                "`# After step 7: <what it built>`",
            ),
            (
                "docs/steps/07/ci.md",
                "# Answered in step 7, on 2026-11-13",
                "`# After step 7: <what it built>`",
            ),
            (
                "docs/steps/tooling-w5/answered.md",
                "# Tooling",
                "`# Answered in <the work's name>, on <date>`",
            ),
            (
                "docs/steps/07/ci.md",
                "",
                "`# After step 7: <what it built>`",
            ),
        ] {
            let found = found(&[(file, &format!("{heading}\n\nText.\n"))]);
            assert_one(&found, &[&format!("{file}:1:"), expected]);
        }
    }

    #[test]
    fn an_open_item_is_named_in_hyphenated_words() {
        for file in [
            "docs/outstanding/Speed-flag.md",
            "docs/outstanding/speed_flag.md",
            "docs/outstanding/speed flag.md",
            "docs/outstanding/speed-flag.txt",
            "docs/outstanding/step-13/speed-flag.md",
            "docs/outstanding/-speed.md",
        ] {
            assert_one(
                &found(&[(file, ITEM)]),
                &["lower-case words joined by hyphens"],
            );
        }
    }

    #[test]
    fn an_open_item_is_the_header_and_one_row() {
        let row = "| **A flag.** | Step 13 | In that step |";
        for (text, line) in [
            // Two items in one file.
            (
                format!("| Item | Who | When |\n|---|---|---|\n{row}\n{row}\n"),
                4,
            ),
            ("| Item | Who | When |\n|---|---|---|\n".to_owned(), 3),
            (
                format!("| Item | Owner | When |\n|---|---|---|\n{row}\n"),
                1,
            ),
            (format!("| Item | Who | When |\n{row}\n"), 2),
            (
                format!("# A flag\n\n| Item | Who | When |\n|---|---|---|\n{row}\n"),
                1,
            ),
            (
                "| Item | Who | When |\n|---|---|---|\nA flag, in prose.\n".to_owned(),
                3,
            ),
            (String::new(), 1),
        ] {
            let found = found(&[("docs/outstanding/speed-flag.md", &text)]);
            assert_one(
                &found,
                &[
                    &format!("docs/outstanding/speed-flag.md:{line}:"),
                    "the table's header",
                ],
            );
        }
    }
}
