//! `cargo xtask mutate`: break the code on purpose, one rule at a time, and
//! see which tests fail (`docs/testing-and-ci.md`, "Breaking the code on
//! purpose").
//!
//! A test that passes says little until it has been seen to fail when the
//! rule it tests is broken. The breaks are data, in `mutations/` (the format
//! is in [`list`]), and this runs them:
//!
//! 1. **Every break is applied to the tree as it is, in memory.** One that
//!    does not apply stops the run before anything is built: its code has
//!    moved, and a break that is not made must never be reported as one no
//!    test caught, or as one a test did. `cargo xtask check` runs this part
//!    alone on every pull request.
//! 2. **The work is done on a copy,** under the target directory, with a
//!    build directory of its own. The working tree is only read.
//! 3. **The tests are run once with nothing broken.** A test that fails by
//!    itself cannot be evidence about a break.
//! 4. **Each break is compiled** before any break's tests are run, and the
//!    ones that do not compile are reported.
//! 5. **Each break's tests are run,** and the tests that failed are read
//!    from nextest's JUnit report. A break no test caught is flagged, and so
//!    is one that the tests named for it did not catch.
//!
//! Every result is written down as it is known, keyed by the code and the
//! break it is for. A run that is stopped, by its own time budget or from
//! outside, carries on from there the next time. The budget is there so that
//! a run through `rx` gives the shared test lock back between slices.

mod drive;
mod jobs;
pub(crate) mod lex;
mod list;

use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::Result;
use crate::workspace;

use list::Break;

const USAGE: &str = "\
usage: cargo xtask mutate [OPTION]... [BREAK]...

Breaks the code on purpose, one break of mutations/*.breaks at a time, on a
copy of the tree, and reports which tests fail. BREAK is a break's name
(`step-12/deny-beats-allow`), a whole list (`step-12`) or the end of a name.
Without one, every break.

  --check        Only check that every break still applies, and the pre-flight:
                 no `find` ending in a comma, no `in` naming two items, no two
                 breaks making one change. Runs nothing.
  --list         Print the breaks chosen, and stop.
  --report       Print what is known so far for the tree as it is. Runs nothing.
  --minutes N    Stop starting new work after N minutes, and carry on from
                 there the next time (5 when not given; 0 for no limit).
  --jobs N       Run N breaks at a time, each in a copy of the tree and a build
                 directory of its own (1 when not given). Each copy's first
                 build is a whole build. The time budget is for all of them
                 together.
  --again        Forget the chosen breaks' results first, and run them again.
                 To carry on after the time ran out, leave it off.
  --no-compile-check
                 Do not compile every break before the first is run.

Exit status: 0 when every chosen break did what its list expects; 1 when one
did not, or something failed; 2 when the time ran out with work left: run
the same command again.";

/// The exit status of a run that stopped at its time budget with work left.
const UNFINISHED: u8 = 2;

/// The minutes a run may start new work for, when it is not told.
const MINUTES: f64 = 5.0;

/// What `cargo xtask mutate` was asked for.
#[derive(Debug, PartialEq)]
struct Request {
    mode: Mode,
    /// The breaks chosen. Empty: all of them.
    only: Vec<String>,
    /// The time budget. `None`: no limit.
    budget: Option<Duration>,
    again: bool,
    compile_check: bool,
    /// How many breaks run at a time.
    jobs: usize,
}

/// The most jobs a run may ask for: each is a whole build directory.
const MOST_JOBS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Run,
    Check,
    List,
    Report,
}

impl Request {
    fn parse(arguments: &[String]) -> Result<Self> {
        let mut request = Self {
            mode: Mode::Run,
            only: Vec::new(),
            budget: Some(Duration::from_secs_f64(MINUTES * 60.0)),
            again: false,
            compile_check: true,
            jobs: 1,
        };
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let mut mode = |mode: Mode| {
                if request.mode == Mode::Run {
                    request.mode = mode;
                    Ok(())
                } else {
                    Err(format!(
                        "`{argument}`: say one of --check, --list and --report"
                    ))
                }
            };
            match argument.as_str() {
                "--check" => mode(Mode::Check)?,
                "--list" => mode(Mode::List)?,
                "--report" => mode(Mode::Report)?,
                "--again" => request.again = true,
                "--no-compile-check" => request.compile_check = false,
                "--jobs" | "-j" => {
                    let text = rest
                        .next()
                        .ok_or_else(|| format!("`{argument}` needs a number\n\n{USAGE}"))?;
                    request.jobs = text
                        .parse()
                        .ok()
                        .filter(|jobs| (1..=MOST_JOBS).contains(jobs))
                        .ok_or_else(|| {
                            format!("`{argument} {text}`: a number from 1 to {MOST_JOBS}")
                        })?;
                }
                "--minutes" => {
                    let text = rest
                        .next()
                        .ok_or_else(|| format!("`--minutes` needs a number\n\n{USAGE}"))?;
                    let not_minutes = || format!("`--minutes {text}` is not a number of minutes");
                    let minutes: f64 = text
                        .parse()
                        .ok()
                        .filter(|minutes: &f64| minutes.is_finite() && *minutes >= 0.0)
                        .ok_or_else(not_minutes)?;
                    request.budget = if minutes > 0.0 {
                        // A number too large to be a length of time is refused
                        // like one that is no number.
                        Some(
                            Duration::try_from_secs_f64(minutes * 60.0)
                                .map_err(|_| not_minutes())?,
                        )
                    } else {
                        None
                    };
                }
                "--help" | "-h" => return Err(USAGE.to_owned()),
                option if option.starts_with('-') => {
                    return Err(format!("unknown option `{option}`\n\n{USAGE}"));
                }
                name => request.only.push(name.to_owned()),
            }
        }
        Ok(request)
    }
}

/// `cargo xtask mutate`
pub(crate) fn main(arguments: &[String]) -> ExitCode {
    let again = arguments.iter().any(|argument| argument == "--again");
    let (status, message) = conclusion(&run(arguments), again);
    if let Some(message) = message {
        eprintln!("xtask: {message}");
    }
    ExitCode::from(status)
}

/// The exit status of a run, and what to say last.
fn conclusion(outcome: &Result<drive::Finished>, again: bool) -> (u8, Option<String>) {
    match outcome {
        Ok(drive::Finished::Complete { flagged: 0 }) => (0, None),
        Ok(drive::Finished::Complete { flagged }) => (
            1,
            Some(format!(
                "{flagged} break(s) did not do what the list expects"
            )),
        ),
        Ok(drive::Finished::Unfinished { left }) => {
            let how = if again {
                // With it, the next run would forget what this one did.
                "Run the same command without `--again` to carry on."
            } else {
                "Run the same command again to carry on."
            };
            (
                UNFINISHED,
                Some(format!(
                    "the time budget ran out with {left} piece(s) of work left. {how}"
                )),
            )
        }
        Err(message) => (1, Some(message.clone())),
    }
}

fn run(arguments: &[String]) -> Result<drive::Finished> {
    let request = Request::parse(arguments)?;
    let root = workspace::root();
    let all = list::load_all(&root)?;
    let done = drive::Finished::Complete { flagged: 0 };

    if request.mode == Mode::Check {
        // The whole list, whatever was chosen: a break that does not apply
        // is wrong whether or not it is run today.
        report_check(&root, &all)?;
        return Ok(done);
    }
    let chosen = choose(&all, &request.only)?;
    if request.mode == Mode::List {
        for chosen in &chosen {
            println!("{}  ({})\n    {}", chosen.id, chosen.place(), chosen.rule);
        }
        println!("{} break(s)", chosen.len());
        return Ok(done);
    }
    if chosen.is_empty() {
        println!("There are no breaks in {}/.", list::DIRECTORY);
        return Ok(done);
    }
    report_check(&root, &all)?;

    let scratch = scratch_directory(&root)?;
    let mut say = |line: &str| println!("{line}");
    if request.mode == Mode::Report {
        return drive::report_only(&root, &scratch, &chosen, &mut say);
    }
    crate::checks::require_nextest()?;
    let started = Instant::now();
    let budget = request.budget;
    let mut runners: Vec<drive::CargoRunner> = (0..request.jobs)
        .map(|job| drive::CargoRunner::new(&scratch, job))
        .collect();
    let mut runners: Vec<&mut dyn drive::Runner> = runners
        .iter_mut()
        .map(|runner| runner as &mut dyn drive::Runner)
        .collect();
    let options = drive::Options {
        again: request.again,
        compile_check: request.compile_check,
    };
    drive::drive_jobs(
        &root,
        &scratch,
        &chosen,
        &options,
        &mut runners,
        &mut || budget.is_some_and(|budget| started.elapsed() >= budget),
        &mut say,
    )
}

/// Where the copy of the tree, its build and the results are kept: under
/// the target directory, which belongs to this checkout alone unless
/// `CARGO_TARGET_DIR` sends several checkouts to one. Then they share the
/// copy, one run at a time: each run brings it up to date first, and
/// results are kept by the code they were found for.
fn scratch_directory(root: &Path) -> Result<PathBuf> {
    Ok(workspace::metadata(root)?.target_directory.join("mutate"))
}

/// The breaks the names choose, in list order. A name chooses a break by
/// its whole id, a list by its name, or every break whose id ends with it.
fn choose(all: &[Break], names: &[String]) -> Result<Vec<Break>> {
    if names.is_empty() {
        return Ok(all.to_vec());
    }
    let mut chosen: Vec<&Break> = Vec::new();
    for name in names {
        let matching: Vec<&Break> = all
            .iter()
            .filter(|one| {
                one.id == *name
                    || one.id.split('/').next() == Some(name.as_str())
                    || one.id.ends_with(&format!("/{name}"))
            })
            .collect();
        if matching.is_empty() {
            return Err(format!(
                "no break is called `{name}`. `cargo xtask mutate --list` prints them."
            ));
        }
        for one in matching {
            if !chosen.iter().any(|already| already.id == one.id) {
                chosen.push(one);
            }
        }
    }
    // In the order of the lists, whatever the order they were asked for in.
    let mut ordered: Vec<Break> = all
        .iter()
        .filter(|one| chosen.iter().any(|chosen| chosen.id == one.id))
        .cloned()
        .collect();
    ordered.dedup_by(|a, b| a.id == b.id);
    Ok(ordered)
}

/// The `breaks` step of `cargo xtask check`: every break of every list
/// still applies to the tree. It reads the files the breaks name and runs
/// nothing.
pub(crate) fn check_breaks_apply() -> Result<()> {
    let root = workspace::root();
    let all = list::load_all(&root)?;
    report_check(&root, &all)
}

fn report_check(root: &Path, breaks: &[Break]) -> Result<()> {
    let problems = check(root, breaks);
    if problems.is_empty() {
        println!(
            "Every break applies: {} in {}/.",
            breaks.len(),
            list::DIRECTORY
        );
        return Ok(());
    }
    for problem in &problems {
        println!("{problem}\n");
    }
    Err(format!(
        "{} break(s) do not apply. Nothing was run.",
        problems.len()
    ))
}

/// What is wrong with the breaks, against the tree at `root`: one message
/// for each break that cannot be made, with its list and line.
fn check(root: &Path, breaks: &[Break]) -> Vec<String> {
    let mut problems = Vec::new();
    // Each file's text and tokens, read once: a list has many breaks of
    // one file, and this runs on every pull request.
    // With the tokens a search uses, and those a comparison does.
    type Read = (String, Vec<lex::Token>, Vec<lex::Token>);
    let mut files: BTreeMap<&str, Option<Read>> = BTreeMap::new();
    // Each change made, by its file and what it does, with the first break
    // that makes it.
    let mut changes: BTreeMap<(&str, Change), String> = BTreeMap::new();
    for one in breaks {
        let read = files.entry(one.file.as_str()).or_insert_with(|| {
            let source = read_source(root, one).ok()?;
            let code = lex::code_tokens(&source);
            let tokens = lex::without_layout_commas(&source, &code);
            Some((source, tokens, code))
        });
        let Some((source, tokens, code)) = read else {
            // Read again for the message, which names this break.
            if let Err(problem) = read_source(root, one) {
                problems.push(problem);
            }
            continue;
        };
        let (range, items) = match locate(one, source, tokens) {
            Ok(located) => located,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        problems.extend(preflight(one, source, tokens, &items));
        let change = change_of(source, code, &range, &one.replace);
        if let Some(first) = changes.get(&(one.file.as_str(), change.clone())) {
            problems.push(format!(
                "{}: break `{}` makes the same change as `{first}`: once the code is read by \
                 token, the two are one break, and a second run of it tests nothing more.\n  \
                 Delete one of them, or make it break the rule its `rule:` names.",
                one.place(),
                one.id
            ));
        } else {
            changes.insert((one.file.as_str(), change), one.id.clone());
        }
        match one.test_packages() {
            Err(problem) => problems.push(problem),
            Ok(packages) => {
                for package in packages {
                    let manifest = if package == "xtask" {
                        root.join("xtask").join("Cargo.toml")
                    } else {
                        root.join("crates").join(&package).join("Cargo.toml")
                    };
                    if !manifest.is_file() {
                        problems.push(format!(
                            "{}: break `{}` runs the tests of `{package}`, and there is no such \
                             crate.\n  Correct its `packages:`.",
                            one.place(),
                            one.id
                        ));
                    }
                }
            }
        }
    }
    problems
}

/// What a change does to a file, read by token: how many tokens it keeps
/// at the start and at the end, and the tokens it puts between them. Two
/// breaks with the same change are one break, whatever their `find` and
/// `replace` say: `a && b` made `a`, and `&& b` made nothing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Change {
    kept_before: usize,
    kept_after: usize,
    put: Vec<String>,
}

/// The change that putting `replace` over the bytes `range` of `source`
/// makes, from the tokens of `source` (`code`, [`lex::code_tokens`]).
///
/// The file with the break made is never lexed: its tokens are those of
/// `source` before the range, `replace`'s, and those of `source` after it.
/// Every break of a list is judged on every pull request, and lexing two
/// whole files for each took the step from 0.2 s to 1.3 s.
fn change_of(source: &str, code: &[lex::Token], range: &Range<usize>, replace: &str) -> Change {
    let before = |index: usize| code[index].text(source);
    let first = code.partition_point(|token| token.start < range.start);
    let after_range = code.partition_point(|token| token.start < range.end);
    let middle = lex::code_texts(replace);
    let length = first + middle.len() + (code.len() - after_range);
    let after = |index: usize| -> &str {
        if index < first {
            before(index)
        } else if index < first + middle.len() {
            middle[index - first]
        } else {
            before(index - first - middle.len() + after_range)
        }
    };
    // The tokens outside the range are the same on both sides by
    // construction: the comparison starts at the range, not at the ends.
    let most = code.len().min(length);
    let kept_before = (first..most)
        .find(|&index| before(index) != after(index))
        .unwrap_or(most);
    let same_after = code.len() - after_range;
    let kept_after = (same_after.min(most - kept_before)..most - kept_before)
        .find(|&back| before(code.len() - 1 - back) != after(length - 1 - back))
        .unwrap_or(most - kept_before);
    Change {
        kept_before,
        kept_after,
        put: (kept_before..length - kept_after)
            .map(|index| after(index).to_owned())
            .collect(),
    }
}

/// What makes a break that applies today stop applying, or apply to code
/// it did not mean, on a later day: the checks of the pre-flight that come
/// after the break is made ([`apply_in`] has the others).
///
/// - Its `find` ends in a comma. `rustfmt` adds and removes the comma
///   before a closing bracket as it wraps and joins lines, and the search
///   passes over such a comma, so the day the code is laid out again the
///   break no longer finds it.
/// - Its `in:` names more than one item. The break is made in the one item
///   whose code it finds today; when the code of another of them changes,
///   it may find that one instead, or both. Not when each of them is under
///   a `#[cfg(…)]` about the system: one function for each system, of
///   which a build has one.
fn preflight(
    one: &Break,
    source: &str,
    tokens: &[lex::Token],
    items: &[Range<usize>],
) -> Vec<String> {
    let mut problems = Vec::new();
    if ends_in_a_layout_comma(&one.find) {
        problems.push(format!(
            "{}: break `{}` has a `find` that ends in a comma, which rustfmt moves: the break \
             stops applying when the line is wrapped or joined.\n  End `find` before the comma.",
            one.place(),
            one.id
        ));
    }
    if let Some(anchor) = &one.anchor
        && items.len() > 1
        && !items
            .iter()
            .all(|item| lex::under_cfg(source, tokens, item.start))
    {
        let lines: Vec<String> = items
            .iter()
            .map(|item| lex::line_of(source, tokens[item.start].start).to_string())
            .collect();
        problems.push(format!(
            "{}: break `{}` has `in: {anchor}`, which names {} items in `{}`, at lines {}.\n  \
             Name the one meant (`Type::method`, or the `impl`'s trait or type), or leave \
             `in` out if `find` is in the file once.",
            one.place(),
            one.id,
            items.len(),
            one.file,
            lines.join(", ")
        ));
    }
    problems
}

/// Whether `find` ends in a comma that `rustfmt` may move: one after an
/// argument, an element or a field. Not the comma after a match arm that is
/// an expression (`Some(1) => one,`), which the formatter always writes.
fn ends_in_a_layout_comma(find: &str) -> bool {
    let texts = lex::code_texts(find);
    let Some((&",", before)) = texts.split_last() else {
        return false;
    };
    // The piece the comma ends: back to the comma before it at the same
    // depth, or to the bracket the piece is inside.
    let mut depth = 0usize;
    let mut start = 0;
    for (index, text) in before.iter().enumerate().rev() {
        match *text {
            ")" | "]" | "}" => depth += 1,
            "(" | "[" | "{" if depth == 0 => {
                start = index + 1;
                break;
            }
            "(" | "[" | "{" => depth -= 1,
            "," if depth == 0 => {
                start = index + 1;
                break;
            }
            _ => {}
        }
    }
    let piece = &before[start..];
    // A match arm has `=>` outside the brackets of its pattern and value.
    let mut depth = 0usize;
    for (index, text) in piece.iter().enumerate() {
        match *text {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            "=" if depth == 0 && piece.get(index + 1) == Some(&">") => return false,
            _ => {}
        }
    }
    true
}

/// The text of the file a break changes.
fn read_source(root: &Path, one: &Break) -> Result<String> {
    let path = root.join(&one.file);
    fs::read_to_string(&path).map_err(|error| {
        format!(
            "{}: break `{}` no longer applies: `{}` cannot be read ({error}).\n  {}",
            one.place(),
            one.id,
            one.file,
            how_to_fix(one)
        )
    })
}

fn how_to_fix(one: &Break) -> String {
    format!(
        "The code this break changes has moved or been rewritten. If the rule it breaks is \
         still in the code (\"{}\"), change the break's `file`, `in` and `find` to the code as \
         it is now. If the rule is gone, delete the break. Then run `cargo xtask mutate \
         --check`, and `cargo xtask mutate {}` to see that its tests still catch it.",
        one.rule, one.id
    )
}

/// The text of a file with the break made. An error says why the break
/// cannot be made, with the break's list and line and what to do about it.
///
/// A break that changes nothing is an error too: it would be reported as
/// one that no test caught.
fn apply(one: &Break, source: &str) -> Result<String> {
    apply_in(one, source, &lex::tokens(source))
}

/// [`apply`], for a source whose tokens are already known.
fn apply_in(one: &Break, source: &str, tokens: &[lex::Token]) -> Result<String> {
    let (range, _) = locate(one, source, tokens)?;
    let mut broken = String::with_capacity(source.len() + one.replace.len());
    broken.push_str(&source[..range.start]);
    broken.push_str(&one.replace);
    broken.push_str(&source[range.end..]);
    Ok(broken)
}

/// Where in `source` a break is made: the bytes its `replace` goes over.
/// An error says why the break cannot be made, as [`apply`] does.
///
/// With the range, the items its `in` names, for the pre-flight.
fn locate(
    one: &Break,
    source: &str,
    tokens: &[lex::Token],
) -> Result<(Range<usize>, Vec<Range<usize>>)> {
    let inside = one
        .anchor
        .as_ref()
        .map(|anchor| format!(" inside `{anchor}`"))
        .unwrap_or_default();
    let located = lex::locate_with_items(
        source,
        tokens,
        one.anchor.as_deref(),
        &one.find,
        one.occurrence,
    );
    let (range, items) = located.map_err(|missing| {
        let (why, fix) = match missing {
            lex::Missing::NothingToFind => (
                "its `find` has no code in it".to_owned(),
                "Give `find` the code to replace.".to_owned(),
            ),
            lex::Missing::NoSuchItem { segment, within } => (
                if within.is_empty() {
                    format!("`{}` has no item called `{segment}`", one.file)
                } else {
                    format!(
                        "`{within}` in `{}` has no item called `{segment}`",
                        one.file
                    )
                },
                how_to_fix(one),
            ),
            lex::Missing::NotFound => (
                format!("its `find` is not in `{}`{inside}", one.file),
                how_to_fix(one),
            ),
            lex::Missing::Ambiguous(lines) => (
                format!(
                    "its `find` is in `{}`{inside} {} times, at lines {}",
                    one.file,
                    lines.len(),
                    lines
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "Make `find` longer, name the item it is in with `in:`, or say which one \
                     is meant with `occurrence:` (counted from 1)."
                    .to_owned(),
            ),
            lex::Missing::NoSuchOccurrence { asked, found } => (
                format!(
                    "it asks for occurrence {asked} of its `find`, which is in `{}`{inside} \
                         {found} time(s)",
                    one.file
                ),
                how_to_fix(one),
            ),
        };
        format!(
            "{}: break `{}` no longer applies: {why}.\n  {fix}",
            one.place(),
            one.id
        )
    })?;
    // Judged on the code that is replaced: the rest of the file is the same
    // text on both sides.
    if lex::same_tokens(&source[range.clone()], &one.replace) {
        return Err(format!(
            "{}: break `{}` changes nothing: its `replace` is the same code as its `find`.\n  \
             Give `replace` the broken code.",
            one.place(),
            one.id
        ));
    }
    Ok((range, items))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::list::{self, Break};
    use super::{Mode, Request, apply, check, choose};

    fn strings(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect()
    }

    pub(super) fn breaks(list: &str, text: &str) -> Vec<Break> {
        list::parse(&format!("mutations/{list}.breaks"), text).unwrap()
    }

    const SOURCE: &str = "\
pub fn over(seconds: f64, budget: f64) -> bool {
    seconds > budget
}

pub fn far_over(seconds: f64, budget: f64) -> bool {
    seconds > 1.5 * budget
}
";

    fn one(body: &str) -> Break {
        let text = format!("[break] b\nrule: The budget is a limit.\nfile: src/lib.rs\n{body}");
        breaks("demo", &text).remove(0)
    }

    #[test]
    fn jobs_are_a_number_from_one_to_sixteen() {
        let jobs =
            |text: &str| Request::parse(&strings(&["--jobs", text])).map(|request| request.jobs);
        assert_eq!(jobs("4"), Ok(4));
        assert_eq!(jobs("16"), Ok(16));
        assert_eq!(Request::parse(&strings(&["-j", "2"])).unwrap().jobs, 2);
        assert_eq!(Request::parse(&[]).unwrap().jobs, 1);
        for wrong in ["0", "17", "many", "-1"] {
            let error = jobs(wrong).unwrap_err();
            assert!(error.contains("a number from 1 to 16"), "{wrong}: {error}");
        }
        assert!(Request::parse(&strings(&["--jobs"])).is_err());
    }

    #[test]
    fn a_break_is_made_where_its_code_is_and_nowhere_else() {
        let broken = apply(
            &one("in: far_over\nfind: seconds > 1.5 * budget\nreplace: seconds > budget\n"),
            SOURCE,
        )
        .unwrap();

        assert_eq!(
            broken,
            SOURCE.replace("seconds > 1.5 * budget", "seconds > budget")
        );
    }

    #[test]
    fn a_break_is_made_whatever_the_formatter_did_to_the_code() {
        let reformatted = SOURCE.replace(
            "    seconds > 1.5 * budget",
            "    seconds\n        > 1.5 // one and a half\n            * budget",
        );

        let broken = apply(
            &one("find: seconds > 1.5 * budget\nreplace: true\n"),
            &reformatted,
        )
        .unwrap();

        assert!(broken.contains("-> bool {\n    true\n}"), "{broken}");
    }

    #[test]
    fn a_break_whose_code_is_gone_says_where_it_is_listed_and_what_to_do() {
        let error = apply(
            &one("in: far_over\nfind: seconds >= budget\nreplace: true\n"),
            SOURCE,
        )
        .unwrap_err();

        assert!(
            error.starts_with(
                "mutations/demo.breaks:1: break `demo/b` no longer applies: its `find` is not \
                 in `src/lib.rs` inside `far_over`."
            ),
            "{error}"
        );
        assert!(error.contains("The budget is a limit."), "{error}");
        assert!(error.contains("delete the break"), "{error}");
        assert!(error.contains("cargo xtask mutate --check"), "{error}");
    }

    #[test]
    fn a_break_that_could_be_made_in_two_places_is_refused() {
        let error = apply(&one("find: seconds >\nreplace: seconds <\n"), SOURCE).unwrap_err();

        assert!(error.contains("2 times, at lines 2, 6"), "{error}");
        assert!(error.contains("`occurrence:`"), "{error}");

        let second = apply(
            &one("find: seconds >\nreplace: seconds <\noccurrence: 2\n"),
            SOURCE,
        )
        .unwrap();
        assert!(second.contains("seconds < 1.5 * budget"), "{second}");
        assert!(second.contains("seconds > budget"), "{second}");
    }

    #[test]
    fn a_missing_item_is_named() {
        let error = apply(&one("in: total\nfind: seconds\nreplace: 0.0\n"), SOURCE).unwrap_err();

        assert!(
            error.contains("`src/lib.rs` has no item called `total`"),
            "{error}"
        );
    }

    #[test]
    fn a_break_that_changes_nothing_is_refused() {
        // The same code, laid out another way.
        let error = apply(
            &one("in: over\nfind: seconds > budget\nreplace:\n    seconds // still\n        > budget\n"),
            SOURCE,
        )
        .unwrap_err();

        assert!(error.contains("changes nothing"), "{error}");
    }

    #[test]
    fn the_check_reads_the_tree_and_names_every_break_that_cannot_be_made() {
        let root = super::drive::tests::tree(
            "check",
            &[
                ("crates/demo/Cargo.toml", "[package]\n"),
                ("crates/demo/src/lib.rs", SOURCE),
            ],
        );
        let list = "\
[break] fine
rule: r
file: crates/demo/src/lib.rs
in: over
find: seconds > budget
replace: true

[break] gone
rule: r
file: crates/demo/src/lib.rs
find: seconds == budget
replace: true

[break] no-file
rule: r
file: crates/demo/src/other.rs
find: x
replace: y

[break] no-crate
rule: r
file: crates/demo/src/lib.rs
in: over
find: seconds > budget
replace: false
packages: cortex-nowhere
";

        let problems = check(&root, &breaks("demo", list));

        assert_eq!(problems.len(), 3, "{problems:#?}");
        assert!(problems[0].starts_with("mutations/demo.breaks:8: break `demo/gone`"));
        assert!(problems[1].starts_with("mutations/demo.breaks:14: break `demo/no-file`"));
        assert!(problems[1].contains("cannot be read"), "{}", problems[1]);
        assert!(problems[2].starts_with("mutations/demo.breaks:20: break `demo/no-crate`"));
        assert!(problems[2].contains("no such crate"), "{}", problems[2]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The source the pre-flight's lists are made against: a call with
    /// arguments, a match, one function for each system, and two items of
    /// one name.
    const PREFLIGHT: &str = "\
pub fn limit(seconds: u64) -> u64 {
    clamp(seconds, 1, 600)
}

pub fn kind(code: u16) -> &'static str {
    match code {
        413 => \"too large\",
        503 => \"overloaded\",
        _ => \"other\",
    }
}

#[cfg(unix)]
fn owner() -> bool {
    uid() == 0
}

#[cfg(windows)]
fn owner() -> bool {
    sid().is_admin()
}

pub struct Budget;

impl Budget {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Budget {
    fn default() -> Self {
        Budget::new()
    }
}

impl fmt::Debug for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(\"budget\")
    }
}
";

    fn problems_with(list: &str) -> Vec<String> {
        let root = super::drive::tests::tree(
            "preflight",
            &[
                ("crates/demo/Cargo.toml", "[package]\n"),
                ("crates/demo/src/lib.rs", PREFLIGHT),
            ],
        );
        let problems = check(&root, &breaks("demo", list));
        let _ = std::fs::remove_dir_all(&root);
        problems
    }

    #[test]
    fn a_list_the_preflight_passes() {
        let list = "\
[break] limit-too-high
rule: r
file: crates/demo/src/lib.rs
find: clamp(seconds, 1, 600)
replace: clamp(seconds, 1, 6000)

[break] an-arm-removed
rule: r
file: crates/demo/src/lib.rs
in: kind
find: 413 => \"too large\",
replace:

[break] owner-anyone
rule: r
file: crates/demo/src/lib.rs
in: owner
find: uid() == 0
replace: true

[break] new-in-default
rule: r
file: crates/demo/src/lib.rs
in: Default::default
find: Budget::new()
replace: Budget

[break] debug-says-other
rule: r
file: crates/demo/src/lib.rs
in: Budget::fmt
find: f.write_str(\"budget\")
replace: f.write_str(\"other\")
";
        assert_eq!(problems_with(list), [] as [String; 0]);
    }

    #[test]
    fn the_preflight_names_each_kind_of_break_that_will_go_wrong() {
        let list = "\
[break] same-code
rule: r
file: crates/demo/src/lib.rs
find: clamp(seconds, 1, 600)
replace: clamp( seconds, 1, /* the same */ 600 )

[break] gone
rule: r
file: crates/demo/src/lib.rs
find: clamp(seconds, 2, 600)
replace: 0

[break] twice
rule: r
file: crates/demo/src/lib.rs
find: Self
replace: Budget

[break] comma
rule: r
file: crates/demo/src/lib.rs
find: seconds, 1,
replace: seconds, 0,

[break] two-items
rule: r
file: crates/demo/src/lib.rs
in: Budget
find: Self
occurrence: 1
replace: Budget

[break] first
rule: r
file: crates/demo/src/lib.rs
find: 600)
replace: 6000)

[break] same-change
rule: r
file: crates/demo/src/lib.rs
in: limit
find: clamp(seconds, 1, 600)
replace: clamp(seconds, 1, 6000)
";
        let problems = problems_with(list);
        let said = |name: &str, what: &str| {
            problems.iter().any(|problem| {
                problem.contains(&format!("`demo/{name}`")) && problem.contains(what)
            })
        };
        assert_eq!(problems.len(), 6, "{problems:#?}");
        assert!(said("same-code", "changes nothing"), "{problems:#?}");
        assert!(said("gone", "is not in"), "{problems:#?}");
        assert!(said("twice", "times, at lines"), "{problems:#?}");
        assert!(
            said("comma", "ends in a comma, which rustfmt moves"),
            "{problems:#?}"
        );
        assert!(
            said("two-items", "names 4 items in `crates/demo/src/lib.rs`"),
            "{problems:#?}"
        );
        assert!(
            said("same-change", "makes the same change as `demo/first`"),
            "{problems:#?}"
        );
    }

    /// Two functions of one name, one under `cfg(test)` and one under a
    /// feature, can both be built: an `in` naming them is refused. One for
    /// each system is not.
    #[test]
    fn only_a_cfg_of_the_system_makes_two_items_of_a_name_one() {
        let source = "#[cfg(test)]\nfn twin() -> u8 { 1 }\n#[cfg(feature = \"x\")]\nfn twin() -> u8 { 2 }\n\
                      #[cfg(target_os = \"linux\")]\nfn os() -> u8 { 3 }\n#[cfg(not(target_os = \"linux\"))]\nfn os() -> u8 { 4 }\n";
        let root = super::drive::tests::tree(
            "preflight_cfg",
            &[
                ("crates/demo/Cargo.toml", "[package]\n"),
                ("crates/demo/src/lib.rs", source),
            ],
        );
        let list = "\
[break] twin
rule: r
file: crates/demo/src/lib.rs
in: twin
find: 2
replace: 5

[break] os
rule: r
file: crates/demo/src/lib.rs
in: os
find: 3
replace: 5
";
        let problems = check(&root, &breaks("demo", list));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(
            problems[0].contains("`demo/twin` has `in: twin`, which names 2 items"),
            "{problems:#?}"
        );
    }

    #[test]
    fn a_comma_is_a_layout_comma_unless_it_ends_a_match_arm() {
        use super::ends_in_a_layout_comma as layout;
        assert!(layout("f(a, b,"));
        assert!(layout("x: 1,"));
        assert!(layout("seconds,"));
        assert!(!layout("f(a, b)"));
        assert!(!layout("Some(413) => Signal::ContextLength,"));
        assert!(!layout("x => f(a, b),"));
        assert!(!layout("'\\u{80}'..='\\u{9f}' => Some(C1),"));
        // An arm's comma ends the arm, not an argument inside its value.
        assert!(layout("x => f(a,"));
    }

    #[test]
    fn breaks_are_chosen_by_name_by_list_or_by_the_end_of_a_name() {
        let body = "rule: r\nfile: f\nfind: x\nreplace: y\n";
        let mut all = breaks("loop", &format!("[break] a\n{body}[break] b\n{body}"));
        all.extend(breaks(
            "policy",
            &format!("[break] a\n{body}[break] c\n{body}"),
        ));
        let ids = |names: &[&str]| -> Vec<String> {
            choose(&all, &strings(names))
                .unwrap()
                .into_iter()
                .map(|one| one.id)
                .collect()
        };

        assert_eq!(ids(&[]), ["loop/a", "loop/b", "policy/a", "policy/c"]);
        assert_eq!(ids(&["policy/c"]), ["policy/c"]);
        assert_eq!(ids(&["loop"]), ["loop/a", "loop/b"]);
        assert_eq!(ids(&["a"]), ["loop/a", "policy/a"]);
        // In list order, and once, however they were asked for.
        assert_eq!(
            ids(&["c", "loop/a", "policy"]),
            ["loop/a", "policy/a", "policy/c"]
        );
        let error = choose(&all, &strings(&["d"])).unwrap_err();
        assert!(error.contains("no break is called `d`"), "{error}");
    }

    #[test]
    fn the_exit_status_says_whether_the_list_held_and_whether_there_is_more_to_do() {
        use super::drive::Finished;

        assert_eq!(
            super::conclusion(&Ok(Finished::Complete { flagged: 0 }), false),
            (0, None)
        );
        let (status, message) = super::conclusion(&Ok(Finished::Complete { flagged: 2 }), false);
        assert_eq!(status, 1);
        assert!(message.unwrap().contains("2 break(s) did not do"));
        let (status, message) = super::conclusion(&Ok(Finished::Unfinished { left: 5 }), false);
        assert_eq!(status, 2);
        assert!(message.unwrap().contains("Run the same command again"));
        let (status, message) = super::conclusion(&Err("no cargo".to_owned()), false);
        assert_eq!((status, message.as_deref()), (1, Some("no cargo")));
    }

    #[test]
    fn a_run_that_forgot_first_is_carried_on_without_forgetting_again() {
        use super::drive::Finished;

        let (status, message) = super::conclusion(&Ok(Finished::Unfinished { left: 5 }), true);

        assert_eq!(status, 2);
        assert!(
            message
                .unwrap()
                .contains("Run the same command without `--again` to carry on."),
        );
    }

    #[test]
    fn a_request_is_read() {
        let request = Request::parse(&strings(&[
            "--minutes",
            "0.5",
            "--again",
            "step-12",
            "--no-compile-check",
        ]))
        .unwrap();

        assert_eq!(
            request,
            Request {
                mode: Mode::Run,
                only: strings(&["step-12"]),
                budget: Some(Duration::from_secs(30)),
                again: true,
                compile_check: false,
                jobs: 1,
            }
        );
        assert_eq!(
            Request::parse(&[]).unwrap().budget,
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            Request::parse(&strings(&["--minutes", "0"]))
                .unwrap()
                .budget,
            None
        );
        assert_eq!(
            Request::parse(&strings(&["--check"])).unwrap().mode,
            Mode::Check
        );
    }

    #[test]
    fn a_request_that_makes_no_sense_is_refused() {
        for (arguments, expected) in [
            (&["--minutes"][..], "needs a number"),
            (&["--minutes", "soon"], "is not a number of minutes"),
            (&["--minutes", "-1"], "is not a number of minutes"),
            (&["--minutes", "1e300"], "is not a number of minutes"),
            (&["--check", "--report"], "say one of"),
            (&["--fast"], "unknown option `--fast`"),
        ] {
            let error = Request::parse(&strings(arguments)).unwrap_err();
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
    }
}
