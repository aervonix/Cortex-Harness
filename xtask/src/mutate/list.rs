//! The break lists: `mutations/<name>.breaks`.
//!
//! A break is one small change that breaks one rule of the code, with the
//! tests that must fail when it is made. The lists are data in the
//! repository, so that a break made once to show that a test bites can be
//! made again by anyone, after any change.
//!
//! # The format
//!
//! One file for each step or crate, so that two steps that add breaks at
//! the same time do not touch the same file. A file is lines of text:
//!
//! ```text
//! # A line that begins with `#` is a comment.
//!
//! [break] cancellation-not-seen-in-wake
//! rule: Cancellation is an input: a wait is ended by the turn being cancelled.
//! file: crates/cortex-agent/src/turn.rs
//! in: wake
//! find: cancelled.as_mut().poll(context).is_ready()
//! replace: cancelled.as_mut().poll(context).is_ready() && false
//! caught-by:
//!     transitions::an_interruption_is_taken_in_every_state_that_waits
//! ```
//!
//! - `[break] <name>` begins a break. The name is lower-case letters,
//!   digits and `-`, and is the break's own within its file. It is shown,
//!   and chosen with `--only`, as `<file>/<name>`.
//! - A key is `key: value` on one line, or `key:` followed by lines that
//!   are indented, which are its value with the indentation taken off.
//! - `rule`: what the break breaks, in a sentence. Required.
//! - `file`: the file to change, relative to the repository. Required.
//! - `in`: the item the code is in: names separated by `::`, such as
//!   `Round::spawn` or `tests::helper`. Optional.
//! - `find`: the code to replace. Required. It is compared by token, so its
//!   layout, its comments and its trailing commas do not matter, and
//!   `rustfmt` cannot make it stop applying. It must be in the file (or the
//!   item) exactly once, unless `occurrence` picks one.
//! - `replace`: what to put there. Required, and may be empty.
//! - `occurrence`: which match of `find` is meant, counted from 1. Optional.
//! - `packages`: the crates whose tests to run, separated by commas.
//!   Optional: without it, the crate the file is in.
//! - `caught-by`: tests that must fail, one a line: a test's name as nextest
//!   prints it (`denials::a_refused_call_stops_the_turn`), or its binary and
//!   its name (`cortex-agent::it denials::…`). A `*` at the end stands for
//!   any ending. Optional: without it any failing test catches the break.
//! - `expect: survives` marks a break that no test can catch because it
//!   changes nothing that can be seen, kept so that the next person does
//!   not try it again. `expect: caught` is what a break is without it.
//!
//! Anything else is an error, with its line: a key that is not known, a key
//! given twice, a break without a required key.

use std::fs;
use std::path::Path;

use crate::Result;

/// The directory of the break lists, relative to the repository.
pub(crate) const DIRECTORY: &str = "mutations";

/// The ending of a break list's name.
const EXTENSION: &str = "breaks";

/// What a break is expected to do to the tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expect {
    /// At least one test fails, and every test of `caught-by` does.
    Caught,
    /// No test fails: the break changes nothing that can be seen.
    Survives,
}

/// One break of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Break {
    /// `<list>/<name>`: `step-12/deny-beats-allow`.
    pub(crate) id: String,
    /// The list it is in, relative to the repository, and the line of its
    /// `[break]`.
    pub(crate) list: String,
    pub(crate) line: usize,
    pub(crate) rule: String,
    pub(crate) file: String,
    pub(crate) anchor: Option<String>,
    pub(crate) find: String,
    pub(crate) replace: String,
    pub(crate) occurrence: Option<usize>,
    /// Empty: the crate the file is in.
    pub(crate) packages: Vec<String>,
    pub(crate) caught_by: Vec<String>,
    pub(crate) expect: Expect,
}

impl Break {
    /// `mutations/step-12.breaks:41`
    pub(crate) fn place(&self) -> String {
        format!("{}:{}", self.list, self.line)
    }

    /// The crates whose tests this break runs: the ones it names, or the
    /// one its file is in.
    pub(crate) fn test_packages(&self) -> Result<Vec<String>> {
        if !self.packages.is_empty() {
            return Ok(self.packages.clone());
        }
        owner_package(&self.file).map(|package| vec![package]).ok_or_else(|| {
            format!(
                "{}: break `{}` changes `{}`, which is in no crate. Say whose tests to run with \
                 `packages:`.",
                self.place(),
                self.id,
                self.file
            )
        })
    }

    /// Everything that decides what the break does, as one text. A result
    /// is kept for exactly this.
    pub(crate) fn definition(&self) -> String {
        format!(
            "{}\n{:?}\n{}\n{}\n{:?}\n{}\n{}\n{:?}",
            self.file,
            self.anchor,
            self.find,
            self.replace,
            self.occurrence,
            self.packages.join(","),
            self.caught_by.join(","),
            self.expect,
        )
    }
}

/// The crate a file belongs to, from where it is: `crates/<name>/...`, or
/// `xtask/...`.
fn owner_package(file: &str) -> Option<String> {
    let mut parts = file.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("crates"), Some(name), Some(_)) => Some(name.to_owned()),
        (Some("xtask"), Some(_), _) => Some("xtask".to_owned()),
        _ => None,
    }
}

/// Reads every break list of the repository at `root`, in name order. No
/// directory is no breaks.
pub(crate) fn load_all(root: &Path) -> Result<Vec<Break>> {
    let directory = root.join(DIRECTORY);
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("could not list {}: {error}", directory.display()))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not list {}: {error}", directory.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if Path::new(&name)
            .extension()
            .is_some_and(|extension| extension == EXTENSION)
        {
            names.push(name);
        }
    }
    names.sort();

    let mut breaks = Vec::new();
    let mut errors = Vec::new();
    for name in names {
        let path = directory.join(&name);
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        match parse(&format!("{DIRECTORY}/{name}"), &text) {
            Ok(list) => breaks.extend(list),
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Ok(breaks)
    } else {
        Err(errors.join("\n"))
    }
}

/// The keys of a break, in the order the format's description gives them.
const KEYS: [&str; 9] = [
    "rule",
    "file",
    "in",
    "find",
    "replace",
    "occurrence",
    "packages",
    "caught-by",
    "expect",
];

/// A break as it is being read: its keys, with the line of each.
struct Draft {
    name: String,
    line: usize,
    values: Vec<(String, usize, String)>,
}

impl Draft {
    fn take(&mut self, key: &str) -> Option<(usize, String)> {
        let index = self.values.iter().position(|(name, _, _)| name == key)?;
        let (_, line, value) = self.values.remove(index);
        Some((line, value))
    }
}

/// Reads one break list. `list` is its path relative to the repository,
/// for the breaks' ids and for errors. Every error of the file is
/// reported, each with its line.
pub(crate) fn parse(list: &str, text: &str) -> Result<Vec<Break>> {
    let stem = Path::new(list)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut errors: Vec<String> = Vec::new();
    let mut error = |line: usize, message: String| errors.push(format!("{list}:{line}: {message}"));

    let mut drafts: Vec<Draft> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let number = index + 1;
        index += 1;
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            error(
                number,
                "an indented line that follows no `key:`. A value of several lines begins \
                 with `key:` alone on a line."
                    .to_owned(),
            );
            continue;
        }
        if let Some(name) = line.strip_prefix("[break]") {
            let name = name.trim();
            let well_formed = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !well_formed {
                error(
                    number,
                    format!(
                        "`[break] {name}`: a break's name is lower-case letters, digits and `-`"
                    ),
                );
            }
            if drafts.iter().any(|draft| draft.name == name) {
                error(
                    number,
                    format!("a second break named `{name}` in this file"),
                );
            }
            drafts.push(Draft {
                name: name.to_owned(),
                line: number,
                values: Vec::new(),
            });
            continue;
        }
        let Some((key, inline)) = line.split_once(':') else {
            error(
                number,
                format!("`{line}` is not `[break] <name>`, `key: value` or a comment"),
            );
            continue;
        };
        let key = key.trim();
        // The lines of a block: indented, or blank between indented ones.
        let mut block: Vec<&str> = Vec::new();
        while index < lines.len()
            && (lines[index].starts_with([' ', '\t']) || lines[index].trim().is_empty())
        {
            block.push(lines[index]);
            index += 1;
        }
        while block.last().is_some_and(|last| last.trim().is_empty()) {
            block.pop();
            index -= 1;
        }
        let inline = inline.trim();
        let value = if block.is_empty() {
            inline.to_owned()
        } else if inline.is_empty() {
            dedent(&block)
        } else {
            error(
                number,
                format!(
                    "`{key}` has a value on its line and indented lines after it. Use one or \
                     the other."
                ),
            );
            continue;
        };
        if !KEYS.contains(&key) {
            error(
                number,
                format!("unknown key `{key}`. The keys are: {}", KEYS.join(", ")),
            );
            continue;
        }
        let Some(draft) = drafts.last_mut() else {
            error(number, format!("`{key}` comes before any `[break]`"));
            continue;
        };
        if draft.values.iter().any(|(name, _, _)| name == key) {
            error(
                number,
                format!("`{key}` is given twice in break `{}`", draft.name),
            );
            continue;
        }
        draft.values.push((key.to_owned(), number, value));
    }

    let mut breaks = Vec::new();
    for mut draft in drafts {
        let mut required = |key: &str, may_be_empty: bool| match draft.take(key) {
            Some((line, value)) if value.trim().is_empty() && !may_be_empty => {
                error(line, format!("`{key}` is empty in break `{}`", draft.name));
                None
            }
            Some((_, value)) => Some(value),
            None => {
                error(draft.line, format!("break `{}` has no `{key}`", draft.name));
                None
            }
        };
        let rule = required("rule", false);
        let file = required("file", false);
        let find = required("find", false);
        let replace = required("replace", true);
        let anchor = draft.take("in").map(|(_, value)| value);
        let occurrence = match draft.take("occurrence") {
            None => None,
            Some((line, value)) => match value.parse::<usize>() {
                Ok(number) if number > 0 => Some(number),
                _ => {
                    error(
                        line,
                        format!("`occurrence: {value}` is not a number counted from 1"),
                    );
                    None
                }
            },
        };
        let packages = draft.take("packages").map_or_else(Vec::new, |(_, value)| {
            value
                .split([',', '\n'])
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        });
        let caught_by: Vec<String> = draft.take("caught-by").map_or_else(Vec::new, |(_, value)| {
            value
                .lines()
                .map(str::trim)
                .filter(|test| !test.is_empty())
                .map(str::to_owned)
                .collect()
        });
        let expect = match draft.take("expect") {
            None => Expect::Caught,
            Some((line, value)) => match value.as_str() {
                "caught" => Expect::Caught,
                "survives" => Expect::Survives,
                other => {
                    error(
                        line,
                        format!("`expect: {other}`: say `caught` or `survives`"),
                    );
                    Expect::Caught
                }
            },
        };
        if expect == Expect::Survives && !caught_by.is_empty() {
            error(
                draft.line,
                format!(
                    "break `{}` is expected to survive and also names tests that catch it",
                    draft.name
                ),
            );
        }
        if let Some(file) = &file
            && (file.starts_with('/')
                || file.contains(['\\', ':'])
                || file.split('/').any(|part| part == ".." || part.is_empty()))
        {
            error(
                draft.line,
                format!(
                    "break `{}`: `file: {file}` must be a path inside the repository, with `/`",
                    draft.name
                ),
            );
        }
        if let (Some(rule), Some(file), Some(find), Some(replace)) = (rule, file, find, replace) {
            breaks.push(Break {
                id: format!("{stem}/{}", draft.name),
                list: list.to_owned(),
                line: draft.line,
                rule,
                file,
                anchor,
                find,
                replace,
                occurrence,
                packages,
                caught_by,
                expect,
            });
        }
    }

    if errors.is_empty() {
        Ok(breaks)
    } else {
        Err(errors.join("\n"))
    }
}

/// The lines of a block without the indentation they share.
fn dedent(block: &[&str]) -> String {
    let indent = block
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = block
        .iter()
        .map(|line| line.get(indent..).unwrap_or("").trim_end())
        .collect();
    lines.join("\n")
}

/// Whether a `caught-by` entry names this test.
pub(crate) fn names_test(entry: &str, binary: &str, name: &str) -> bool {
    let matches = |pattern: &str, text: &str| match pattern.strip_suffix('*') {
        Some(prefix) => text.starts_with(prefix),
        None => text == pattern,
    };
    match entry.split_once(char::is_whitespace) {
        Some((entry_binary, entry_name)) => {
            entry_binary == binary && matches(entry_name.trim(), name)
        }
        None => matches(entry, name),
    }
}

#[cfg(test)]
mod tests {
    use super::{Break, Expect, names_test, parse};

    const LIST: &str = "\
# The loop's breaks.

[break] cancel-wins
rule: A cancelled turn is seen before a report that is also ready.
file: crates/cortex-agent/src/turn.rs
in: wake
find:
    if cancelled.as_mut().poll(context).is_ready() {

        return Poll::Ready(Woke::Cancelled);
    }
replace:
    let _ = &mut cancelled;
caught-by:
    interrupt::a_cancelled_turn_starts_nothing
    cortex-agent::it interrupt::sys_*

# Nothing can see this one.
[break] no-difference
rule: The order of two independent writes.
file: xtask/src/steps.rs
find: results.push((step.name, elapsed, outcome));
replace:
occurrence: 1
packages: xtask, cortex-base
expect: survives
";

    #[test]
    fn a_list_is_read() {
        let breaks = parse("mutations/loop.breaks", LIST).unwrap();

        assert_eq!(
            breaks,
            [
                Break {
                    id: "loop/cancel-wins".to_owned(),
                    list: "mutations/loop.breaks".to_owned(),
                    line: 3,
                    rule: "A cancelled turn is seen before a report that is also ready.".to_owned(),
                    file: "crates/cortex-agent/src/turn.rs".to_owned(),
                    anchor: Some("wake".to_owned()),
                    find: "if cancelled.as_mut().poll(context).is_ready() {\n\n    return \
                           Poll::Ready(Woke::Cancelled);\n}"
                        .to_owned(),
                    replace: "let _ = &mut cancelled;".to_owned(),
                    occurrence: None,
                    packages: vec![],
                    caught_by: vec![
                        "interrupt::a_cancelled_turn_starts_nothing".to_owned(),
                        "cortex-agent::it interrupt::sys_*".to_owned(),
                    ],
                    expect: Expect::Caught,
                },
                Break {
                    id: "loop/no-difference".to_owned(),
                    list: "mutations/loop.breaks".to_owned(),
                    line: 19,
                    rule: "The order of two independent writes.".to_owned(),
                    file: "xtask/src/steps.rs".to_owned(),
                    anchor: None,
                    find: "results.push((step.name, elapsed, outcome));".to_owned(),
                    replace: String::new(),
                    occurrence: Some(1),
                    packages: vec!["xtask".to_owned(), "cortex-base".to_owned()],
                    caught_by: vec![],
                    expect: Expect::Survives,
                },
            ]
        );
        assert_eq!(breaks[0].place(), "mutations/loop.breaks:3");
    }

    #[test]
    fn a_break_runs_the_tests_of_the_crate_its_file_is_in() {
        let breaks = parse("mutations/loop.breaks", LIST).unwrap();

        assert_eq!(breaks[0].test_packages().unwrap(), ["cortex-agent"]);
        assert_eq!(breaks[1].test_packages().unwrap(), ["xtask", "cortex-base"]);
    }

    #[test]
    fn a_file_in_no_crate_needs_its_packages_named() {
        let list = "[break] a\nrule: r\nfile: .config/nextest.toml\nfind: x\nreplace: y\n";
        let breaks = parse("mutations/a.breaks", list).unwrap();

        let error = breaks[0].test_packages().unwrap_err();

        assert!(error.contains("mutations/a.breaks:1"), "{error}");
        assert!(error.contains("`packages:`"), "{error}");
    }

    #[test]
    fn a_change_to_a_break_changes_its_definition_and_its_rule_does_not() {
        let breaks = parse("mutations/loop.breaks", LIST).unwrap();
        let mut reworded = breaks[0].clone();
        reworded.rule = "Said another way.".to_owned();
        reworded.line = 90;
        let mut changed = breaks[0].clone();
        changed.replace = "let _ = &mut cancelled; let _ = 1;".to_owned();

        assert_eq!(reworded.definition(), breaks[0].definition());
        assert_ne!(changed.definition(), breaks[0].definition());
    }

    /// The errors of a list, which must be all of `expected`, in order.
    fn errors(list: &str, expected: &[&str]) {
        let error = parse("mutations/x.breaks", list).unwrap_err();
        let lines: Vec<&str> = error.lines().collect();
        assert_eq!(lines.len(), expected.len(), "{error}");
        for (line, expected) in lines.iter().zip(expected) {
            assert!(line.contains(expected), "`{expected}` is not in: {line}");
        }
    }

    #[test]
    fn what_is_missing_is_named_with_the_line_of_the_break() {
        errors(
            "# nothing\n[break] a\nrule: r\n",
            &[
                "mutations/x.breaks:2: break `a` has no `file`",
                "mutations/x.breaks:2: break `a` has no `find`",
                "mutations/x.breaks:2: break `a` has no `replace`",
            ],
        );
        errors(
            "[break] a\nrule:\nfile: f\nfind:\nreplace:\n",
            &[
                "mutations/x.breaks:2: `rule` is empty in break `a`",
                "mutations/x.breaks:4: `find` is empty in break `a`",
            ],
        );
    }

    #[test]
    fn what_is_not_understood_is_refused_with_its_line() {
        let head = "[break] a\nrule: r\nfile: f\nfind: x\nreplace: y\n";
        errors(
            &format!("{head}catches: t\n"),
            &["mutations/x.breaks:6: unknown key `catches`. The keys are: rule, file, in"],
        );
        errors(
            &format!("{head}find: z\n"),
            &["mutations/x.breaks:6: `find` is given twice in break `a`"],
        );
        errors(
            &format!("{head}expect: maybe\n"),
            &["mutations/x.breaks:6: `expect: maybe`: say `caught` or `survives`"],
        );
        errors(
            &format!("{head}occurrence: 0\n"),
            &["mutations/x.breaks:6: `occurrence: 0` is not a number counted from 1"],
        );
        errors(
            &format!("{head}just some words\n"),
            &["mutations/x.breaks:6: `just some words` is not `[break] <name>`"],
        );
        errors(
            &format!("{head}find: x\n    and more\n"),
            &["mutations/x.breaks:6: `find` has a value on its line and indented lines"],
        );
        errors(
            "rule: r\n",
            &["mutations/x.breaks:1: `rule` comes before any `[break]`"],
        );
        errors(
            "    stray\n",
            &["mutations/x.breaks:1: an indented line that follows no `key:`"],
        );
    }

    #[test]
    fn a_name_is_well_formed_and_its_own() {
        let body = "rule: r\nfile: f\nfind: x\nreplace: y\n";
        errors(
            &format!("[break] Not This\n{body}"),
            &["mutations/x.breaks:1: `[break] Not This`: a break's name is"],
        );
        errors(
            &format!("[break]\n{body}"),
            &["mutations/x.breaks:1: `[break] `: a break's name is"],
        );
        errors(
            &format!("[break] a\n{body}[break] a\n{body}"),
            &["mutations/x.breaks:6: a second break named `a` in this file"],
        );
    }

    #[test]
    fn a_file_outside_the_repository_is_refused() {
        for file in [
            "/etc/passwd",
            "../other/src/lib.rs",
            "crates\\x\\src\\lib.rs",
            "a/../../b",
            // A drive: on Windows a path of its own wherever it is joined.
            "C:/Windows/system.ini",
            "C:system.ini",
            "crates//x.rs",
        ] {
            let list = format!("[break] a\nrule: r\nfile: {file}\nfind: x\nreplace: y\n");
            errors(&list, &["must be a path inside the repository"]);
        }
    }

    #[test]
    fn a_break_that_survives_names_no_test() {
        errors(
            "[break] a\nrule: r\nfile: f\nfind: x\nreplace: y\nexpect: survives\ncaught-by: t\n",
            &["mutations/x.breaks:1: break `a` is expected to survive and also names tests"],
        );
    }

    #[test]
    fn a_block_keeps_its_own_indentation_and_its_blank_lines() {
        let list = "[break] a\nrule: r\nfile: f\nfind:\n\tif x {\n\t\ty();\n\n\t}\n\nreplace:\n    {\n        z\n    }\n";

        let breaks = parse("mutations/x.breaks", list).unwrap();

        assert_eq!(breaks[0].find, "if x {\n\ty();\n\n}");
        assert_eq!(breaks[0].replace, "{\n    z\n}");
    }

    #[test]
    fn an_attribute_in_a_block_is_not_a_comment() {
        let list =
            "[break] a\nrule: r\nfile: f\nfind:\n    #[cfg(windows)]\n    fn x() {}\nreplace:\n";

        let breaks = parse("mutations/x.breaks", list).unwrap();

        assert_eq!(breaks[0].find, "#[cfg(windows)]\nfn x() {}");
    }

    #[test]
    fn a_test_is_named_by_its_name_or_with_its_binary() {
        let binary = "cortex-agent::it";
        let name = "denials::a_refused_call_stops_the_turn";

        assert!(names_test(name, binary, name));
        assert!(names_test(&format!("{binary} {name}"), binary, name));
        assert!(names_test("denials::*", binary, name));
        assert!(names_test(
            "cortex-agent::it denials::a_refused*",
            binary,
            name
        ));
        // A name is the whole name.
        assert!(!names_test("denials::a_refused_call", binary, name));
        assert!(!names_test("a_refused_call_stops_the_turn", binary, name));
        assert!(!names_test(&format!("cortex-agent {name}"), binary, name));
        assert!(!names_test("rounds::*", binary, name));
    }
}
