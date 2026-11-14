//! The rules read from the Rust source itself, by token
//! ([`crate::mutate::lex`]): comments and strings are never taken for
//! code.
//!
//! - **A wrapped trait has no default bodies.** [`WRAPPED`] names the
//!   traits that are wrapped (a type that holds another implementation and
//!   forwards to it). A method added to one later with a default body would
//!   be silently not forwarded by a wrapper that predates it: the default
//!   would run in its place, and the build would not say so. Step 26's
//!   wrapper of `Tool` did that with `Tool::preview`. Without defaults,
//!   adding a method fails to build until every implementation, wrappers
//!   included, writes it.
//! - **A program is not started by a bare name** outside tests: a
//!   `Command::new("git")` searches the `PATH`, relative entries and all,
//!   in the child's working directory, which is the workspace. The harness
//!   finds a program with `cortex_base::program` and starts it by the
//!   absolute path found (step 13a's review, R4). [`STARTERS`] names the
//!   calls that start what they are given.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::Violation;
use crate::mutate::lex::{self, Token};

/// The traits that are wrapped, and the file each is declared in.
pub(super) const WRAPPED: [(&str, &str); 3] = [
    ("crates/cortex-tools/src/contract.rs", "Tool"),
    ("crates/cortex-tools/src/process/mod.rs", "Launcher"),
    ("crates/cortex-agent/src/policy.rs", "Policy"),
];

/// The calls that start the program they are given as their first
/// argument, by the last two segments of their path:
/// `std::process::Command::new` and `tokio::process::Command::new` are both
/// `Command::new`. A function or type that starts what it is given joins the
/// list.
const STARTERS: [&str; 5] = [
    "Command::new",
    "CommandLine::new",
    "Shell::new",
    "brief::run",
    "Pty::spawn",
];

/// Whether a file's text calls a starter: the files that do not are not
/// lexed, which keeps the rule cheap enough for every pull request.
/// `rustfmt` never breaks `Command::new(` across lines.
fn calls_a_starter(source: &str) -> bool {
    STARTERS
        .iter()
        .any(|starter| source.contains(&format!("{starter}(")))
}

/// What a file's text must hold to declare a module under a test `cfg`.
const TEST_CFG: [&str; 2] = ["cfg(test)", "cfg(all(test"];

/// Whether a file's text may declare a module under a test `cfg` that is
/// a file of its own (`#[cfg(test)]` then `mod name;`): the files that do
/// not are not lexed for it.
fn may_declare_a_test_module(source: &str) -> bool {
    TEST_CFG.iter().any(|cfg| {
        source.match_indices(cfg).any(|(at, _)| {
            let after = &source[at..];
            let Some(close) = after.find(']') else {
                return false;
            };
            let rest = after[close + 1..].trim_start();
            let rest = rest.strip_prefix("pub ").map_or(rest, str::trim_start);
            rest.strip_prefix("mod ").is_some_and(|rest| {
                rest.find(|c: char| !c.is_alphanumeric() && c != '_')
                    .is_some_and(|end| rest[end..].trim_start().starts_with(';'))
            })
        })
    })
}

/// What marks one call as allowed, on its line or the line before, with
/// the reason after it.
const ALLOW: &str = "// program by name:";

/// The places a bare name is allowed in, as a whole, and why.
const ALLOWED_PLACES: [(&str, &str); 2] = [
    (
        "xtask/",
        "the developer's commands, which run the developer's own git, gh and rustc from their \
         PATH as cargo does, in a checkout they chose",
    ),
    (
        "crates/cortex-testkit/",
        "only tests depend on the test kit (`cargo xtask rules` checks it), and a test runs the \
         machine's tools",
    ),
];

/// Every Rust source file the rules read: the workspace's crates and
/// `xtask`.
pub(super) fn is_read(file: &str) -> bool {
    file.ends_with(".rs")
        && (file.starts_with("crates/") || file.starts_with("xtask/") || file.starts_with("fuzz/"))
}

/// Both rules, over every file [`is_read`] takes.
pub(super) fn check(sources: &BTreeMap<String, String>) -> Vec<Violation> {
    let mut violations = wrapped_traits(sources);
    let test_files = test_module_files(sources);
    for (file, source) in sources {
        if !test_files.contains(file) && calls_a_starter(source) {
            violations.extend(bare_programs(file, source, false));
        }
    }
    violations
}

// ---------------------------------------------------------- wrapped traits

/// Every method with a default body in the traits of [`WRAPPED`].
pub(super) fn wrapped_traits(sources: &BTreeMap<String, String>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (file, name) in WRAPPED {
        let Some(source) = sources.get(file) else {
            violations.push(Violation::new(
                file,
                None,
                format!(
                    "the trait `{name}` is not here: the file is gone. Name where it is now in \
                     `WRAPPED` (xtask/src/rules/rust_source.rs)."
                ),
            ));
            continue;
        };
        violations.extend(defaults_of(file, source, name));
    }
    violations
}

/// The methods of trait `name` in `source` that have a default body.
fn defaults_of(file: &str, source: &str, name: &str) -> Vec<Violation> {
    let all = lex::tokens(source);
    let text = |index: usize| all[index].text(source);
    let Some(body) = (0..all.len().saturating_sub(1))
        .find(|&index| text(index) == "trait" && text(index + 1) == name)
        .and_then(|index| body_after(source, &all, index))
    else {
        return vec![Violation::new(
            file,
            None,
            format!(
                "the trait `{name}` is not declared here. Name where it is now in `WRAPPED` \
                 (xtask/src/rules/rust_source.rs), or take it off the list if nothing wraps it."
            ),
        )];
    };
    let mut violations = Vec::new();
    let mut depth = 0usize;
    let mut index = body.start;
    while index < body.end {
        match text(index) {
            "{" | "(" | "[" => depth += 1,
            "}" | ")" | "]" => depth = depth.saturating_sub(1),
            "fn" if depth == 0 && index + 1 < body.end => {
                let method = text(index + 1).to_owned();
                // The signature ends at `;` (no body) or at a `{` outside
                // its brackets (a default body).
                let mut inner = 0usize;
                let mut at = index + 1;
                while at < body.end {
                    match text(at) {
                        "(" | "[" => inner += 1,
                        ")" | "]" => inner = inner.saturating_sub(1),
                        ";" if inner == 0 => break,
                        "{" if inner == 0 => {
                            violations.push(Violation::new(
                                file,
                                Some(lex::line_of(source, all[index].start)),
                                format!(
                                    "`{name}::{method}` has a default body. `{name}` is wrapped, \
                                     and a wrapper written before the method was added would \
                                     inherit the default silently instead of forwarding it. Take \
                                     the body out, and write the method in every `impl {name} \
                                     for`: the leaf implementations with what the default did, \
                                     each wrapper by calling the one it wraps \
                                     (docs/testing-and-ci.md, \"Lints and formatting\")."
                                ),
                            ));
                            break;
                        }
                        _ => {}
                    }
                    at += 1;
                }
                index = at;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    violations
}

/// The tokens inside the braces of the item whose keyword is at `start`:
/// from the first `{` after it that is outside brackets and angle brackets
/// to its match.
fn body_after(source: &str, all: &[Token], start: usize) -> Option<Range<usize>> {
    let open = (start..all.len()).find(|&index| all[index].text(source) == "{")?;
    let close = matching(source, all, open)?;
    Some(open + 1..close)
}

/// The index of the bracket that closes the one at `open`.
fn matching(source: &str, all: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in all.iter().enumerate().skip(open) {
        match token.text(source) {
            "{" | "(" | "[" => depth += 1,
            "}" | ")" | "]" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

// ----------------------------------------------------------- bare programs

/// Every call of [`STARTERS`] in `source` that is given a bare name: a
/// string with no path separator in it, written in the call or as a
/// constant of the same file. Not in tests: a file under a `tests`
/// directory, a module under `#[cfg(test)]` (`test_file` says the whole
/// file is one), or the places of [`ALLOWED_PLACES`].
pub(super) fn bare_programs(file: &str, source: &str, test_file: bool) -> Vec<Violation> {
    if test_file
        || file.split('/').any(|part| part == "tests")
        || ALLOWED_PLACES
            .iter()
            .any(|(place, _)| file.starts_with(place))
    {
        return Vec::new();
    }
    let all = lex::tokens(source);
    let text = |index: usize| all[index].text(source);
    let tests = test_modules(source, &all);
    let constants = string_constants(source, &all);
    let lines: Vec<&str> = source.lines().collect();
    let mut violations = Vec::new();
    for (index, token) in all.iter().enumerate().take(all.len().saturating_sub(5)) {
        let starter = format!("{}::{}", text(index), text(index + 3));
        if text(index + 1) != ":"
            || text(index + 2) != ":"
            || text(index + 4) != "("
            || !STARTERS.contains(&starter.as_str())
            || tests.iter().any(|range| range.contains(&index))
        {
            continue;
        }
        // The first argument: up to the `,` or `)` that ends it.
        let mut depth = 0usize;
        let mut end = index + 5;
        while end < all.len() {
            match text(end) {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" if depth == 0 => break,
                ")" | "]" | "}" => depth -= 1,
                "," if depth == 0 => break,
                _ => {}
            }
            end += 1;
        }
        let argument: Vec<&str> = (index + 5..end).map(text).collect();
        let Some(name) = name_of(&argument, &constants) else {
            continue;
        };
        if name.is_empty() || name.contains(['/', '\\']) {
            continue;
        }
        let line = lex::line_of(source, token.start);
        let allowed = |line: usize| {
            line.checked_sub(1)
                .and_then(|at| lines.get(at))
                .and_then(|text| text.split_once(ALLOW))
                .is_some_and(|(_, reason)| !reason.trim().is_empty())
        };
        if allowed(line) || allowed(line - 1) {
            continue;
        }
        violations.push(Violation::new(
            file,
            Some(line),
            format!(
                "`{starter}(\"{name}\")` starts a program by its bare name, which is looked for \
                 in the PATH's relative entries and, on Windows, in the working directory: a \
                 `{name}` in the workspace would run in its place. Find it with \
                 `cortex_base::program` (`program::git()`, or `program::find` over \
                 `program::path_directories()`) and start it by the path found. If a bare name is \
                 right here, say why on the line before: `{ALLOW} <reason>`."
            ),
        ));
    }
    violations
}

/// The text a call's first argument stands for, when it is a string the
/// file spells out: a literal, as it is or borrowed (`&"git"`), made into a
/// `String`, an `OsStr` or a path (`"git".to_owned()`, `String::from("git")`,
/// `OsStr::new("git")`), or a constant or `let` of the file that holds one.
fn name_of(argument: &[&str], constants: &BTreeMap<String, String>) -> Option<String> {
    let argument = match argument {
        ["&", rest @ ..] => rest,
        all => all,
    };
    match argument {
        [one] => string_value(one).or_else(|| constants.get(*one).cloned()),
        [literal, ".", "to_owned" | "to_string" | "into", "(", ")"] => string_value(literal),
        [
            "String" | "OsStr" | "OsString" | "Path" | "PathBuf",
            ":",
            ":",
            "from" | "new",
            "(",
            literal,
            ")",
        ] => string_value(literal),
        _ => None,
    }
}

/// What a string literal holds, for the simple ones: `"…"` and `r"…"`,
/// `r#"…"#`, with no escape in it. `None` for anything else.
fn string_value(token: &str) -> Option<String> {
    let raw = token.strip_prefix('r');
    let text = raw.unwrap_or(token).trim_matches('#');
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    (raw.is_some() || !inner.contains('\\')).then(|| inner.to_owned())
}

/// The names of `source` that hold a string: `const NAME: &str = "…";`,
/// and `let name = "…";` anywhere in the file (a name bound twice keeps the
/// last).
fn string_constants(source: &str, all: &[Token]) -> BTreeMap<String, String> {
    let text = |index: usize| all[index].text(source);
    let mut constants = BTreeMap::new();
    for index in 0..all.len().saturating_sub(4) {
        if text(index) == "let"
            && text(index + 2) == "="
            && text(index + 4) == ";"
            && let Some(value) = string_value(text(index + 3))
        {
            constants.insert(text(index + 1).to_owned(), value);
        }
    }
    for index in 0..all.len().saturating_sub(6) {
        if text(index) == "const"
            && text(index + 2) == ":"
            && text(index + 3) == "&"
            && text(index + 4) == "str"
            && text(index + 5) == "="
            && let Some(value) = string_value(text(index + 6))
        {
            constants.insert(text(index + 1).to_owned(), value);
        }
    }
    constants
}

/// Whether the attribute that starts at `index` (`#`) is a `cfg` that holds
/// only under test: `#[cfg(test)]` or `#[cfg(all(test, …))]`.
fn is_test_cfg(source: &str, all: &[Token], index: usize) -> bool {
    let words: Vec<&str> = all[index..all.len().min(index + 7)]
        .iter()
        .map(|token| token.text(source))
        .collect();
    matches!(
        words.as_slice(),
        ["#", "[", "cfg", "(", "test", ")", ..] | ["#", "[", "cfg", "(", "all", "(", "test"]
    )
}

/// The token ranges of the modules and functions under a test `cfg`, and
/// of every `#[test]` function.
fn test_modules(source: &str, all: &[Token]) -> Vec<Range<usize>> {
    let text = |index: usize| all[index].text(source);
    let mut ranges = Vec::new();
    for index in 0..all.len() {
        let test_attribute = text(index) == "#"
            && (is_test_cfg(source, all, index)
                || (index + 3 < all.len()
                    && text(index + 1) == "["
                    && text(index + 2) == "test"
                    && text(index + 3) == "]"));
        if !test_attribute {
            continue;
        }
        // The item it is on: up to the next `;` (a module in a file of its
        // own: [`test_module_files`]), or the end of its first braces.
        let Some(end) = (index..all.len()).find(|&at| matches!(text(at), ";" | "{")) else {
            continue;
        };
        if text(end) == "{"
            && let Some(close) = matching(source, all, end)
        {
            ranges.push(index..close + 1);
        }
    }
    ranges
}

/// The files that are a module under `#[cfg(test)]`, declared with
/// `mod name;`: `name.rs` or `name/mod.rs` beside a `lib.rs`, `main.rs` or
/// `mod.rs`, or in the folder named for any other file.
pub(super) fn test_module_files(sources: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    for (file, source) in sources {
        if !may_declare_a_test_module(source) {
            continue;
        }
        let all = lex::tokens(source);
        let text = |index: usize| all[index].text(source);
        let Some((folder, stem)) = file.rsplit_once('/') else {
            continue;
        };
        let stem = stem.trim_end_matches(".rs");
        let base = if matches!(stem, "lib" | "main" | "mod") {
            folder.to_owned()
        } else {
            format!("{folder}/{stem}")
        };
        for index in 0..all.len() {
            if text(index) != "#" || !is_test_cfg(source, all.as_slice(), index) {
                continue;
            }
            // `#[cfg(test)]`, its `]`, then `mod name ;`.
            let Some(close) = (index..all.len()).find(|&at| text(at) == "]") else {
                continue;
            };
            let mut at = close + 1;
            if at < all.len() && text(at) == "pub" {
                at += 1;
            }
            if at + 2 < all.len() && text(at) == "mod" && text(at + 2) == ";" {
                let name = text(at + 1);
                files.insert(format!("{base}/{name}.rs"));
                files.insert(format!("{base}/{name}/mod.rs"));
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::{bare_programs, check, defaults_of, test_module_files, wrapped_traits};

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/rules")
                .join(name),
        )
        .unwrap()
    }

    fn lines(violations: &[super::Violation]) -> Vec<usize> {
        violations.iter().filter_map(|found| found.line).collect()
    }

    #[test]
    fn a_trait_with_no_default_bodies_passes_and_each_default_is_named() {
        let clean = "pub trait Tool: Send {\n    fn spec(&self) -> &Spec;\n    \
                     fn run<'a>(&'a self, f: impl Fn() -> u8) -> Box<dyn Future<Output = ()> + 'a>;\n}\n\
                     fn free() { let _ = 1; }\n";
        assert_eq!(defaults_of("f.rs", clean, "Tool"), []);

        let defaults = "pub trait Tool {\n    fn spec(&self) -> u8;\n    /// It has { braces } in a comment.\n    \
                        fn preview(&self, plan: &[u8; 2]) -> Option<u8> {\n        None\n    }\n    \
                        fn added(&self) where Self: Sized {}\n}\n";
        let found = defaults_of("f.rs", defaults, "Tool");
        assert_eq!(lines(&found), [4, 7]);
        assert!(
            found[0]
                .message
                .contains("`Tool::preview` has a default body")
        );
        assert!(found[1].message.contains("`Tool::added`"));
    }

    #[test]
    fn a_wrapped_trait_that_moved_is_an_error_not_a_pass() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "crates/cortex-tools/src/contract.rs".to_owned(),
            "pub trait Other {}\n".to_owned(),
        );
        let found = wrapped_traits(&sources);
        // Tool is not in its file; the other two files are not there.
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].message.contains("`Tool` is not declared here"));
        assert!(found[1].message.contains("the file is gone"));
    }

    #[test]
    fn the_wrapped_traits_as_they_are_have_no_default_bodies() {
        let root = crate::workspace::root();
        let mut sources = BTreeMap::new();
        for (file, _) in super::WRAPPED {
            sources.insert(
                file.to_owned(),
                std::fs::read_to_string(root.join(file)).unwrap(),
            );
        }
        assert_eq!(wrapped_traits(&sources), []);
    }

    #[test]
    fn programs_started_by_a_path_or_in_tests_pass() {
        let found = bare_programs(
            "crates/cortex-demo/src/lib.rs",
            &fixture("bare-programs-pass.rs"),
            false,
        );
        assert_eq!(found, [], "{found:#?}");
    }

    #[test]
    fn each_program_started_by_a_bare_name_is_named_with_its_line() {
        let source = fixture("bare-programs-fail.rs");
        let found = bare_programs("crates/cortex-demo/src/lib.rs", &source, false);
        let expected: Vec<usize> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains("// flagged"))
            .map(|(index, _)| index + 1)
            .collect();
        assert_eq!(lines(&found), expected, "{found:#?}");
        assert!(
            found[0]
                .message
                .contains("`Command::new(\"git\")` starts a program")
        );
        assert!(found[0].message.contains("cortex_base::program"));
        // The same file in a test, in the test kit or in xtask passes.
        assert_eq!(
            bare_programs("crates/cortex-demo/tests/it/main.rs", &source, false),
            []
        );
        assert_eq!(
            bare_programs("crates/cortex-demo/src/lib.rs", &source, true),
            []
        );
        assert_eq!(
            bare_programs("crates/cortex-testkit/src/lib.rs", &source, false),
            []
        );
        assert_eq!(bare_programs("xtask/src/main.rs", &source, false), []);
    }

    #[test]
    fn a_module_under_cfg_test_in_a_file_of_its_own_is_a_test() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "crates/a/src/lib.rs".to_owned(),
            "#[cfg(test)]\nmod tests;\nmod shell;\n".to_owned(),
        );
        let started = "fn f() { std::process::Command::new(\"sh\"); }\n";
        sources.insert(
            "crates/a/src/shell.rs".to_owned(),
            format!("#[cfg(all(test, unix))]\npub mod cases;\n{started}"),
        );
        for file in [
            "crates/a/src/tests.rs",
            "crates/a/src/shell/cases.rs",
            "crates/a/src/shell.rs",
        ] {
            sources
                .entry(file.to_owned())
                .or_insert_with(|| started.to_owned());
        }
        let files = test_module_files(&sources);
        assert!(files.contains("crates/a/src/tests.rs"));
        assert!(files.contains("crates/a/src/shell/cases.rs"));
        assert!(!files.contains("crates/a/src/shell.rs"));
        // So only the module that is not a test is flagged.
        let found = check(&sources)
            .into_iter()
            .filter(|found| found.message.contains("bare name"))
            .map(|found| found.file)
            .collect::<Vec<_>>();
        assert_eq!(found, ["crates/a/src/shell.rs"]);
    }
}
