//! `cargo xtask owner-guard`: fails when a change carries one of the owner's
//! personal details (rule 3 of `docs/build-plan.md`; `docs/testing-and-ci.md`,
//! "The owner guard").
//!
//! The details themselves are not in the repository, which is public: they
//! are terms in a secret the owner holds, `OWNER_TERMS`, which the workflow
//! puts in the environment of this one step. The command looks for them in
//! what a pull request, or a push to the main branch, adds:
//!
//! - every line the diff adds, and the path of every file it adds or changes;
//! - every commit's message, and its author's and committer's names and
//!   emails.
//!
//! **A term is never printed.** A finding names the file and the line, or
//! the commit and the field, and says that it matches a protected term.
//! GitHub masks a secret's value in a log, but nothing here relies on it.
//!
//! **Without terms it passes, and says so** with a warning in the log and on
//! the job's summary page: a pull request from a fork is not given the
//! repository's secrets, and the secret may not have been set yet.

use std::env;
use std::path::Path;
use std::process::Command;

use crate::Result;
use crate::steps;

/// The environment variable the workflow fills from the secret.
const TERMS: &str = "OWNER_TERMS";

/// What `git` calls a tree with nothing in it: the base of a first push.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const USAGE: &str = "\
usage: cargo xtask owner-guard --base REV --head REV

Fails when the change from REV to REV adds a term of OWNER_TERMS: in an
added line, a file's path, a commit message, or a commit's author or
committer. A base of forty zeros, as a first push has, checks the head
commit alone. OWNER_TERMS holds one term a line: a plain word or phrase is
matched as a whole word, and a term with `@` or `/` in it anywhere in the
text; case is ignored. A term is never printed.";

/// The terms, ready to match: lowered, and each with its kind.
#[derive(Debug)]
pub(crate) struct Terms {
    terms: Vec<Term>,
}

#[derive(Debug)]
struct Term {
    lowered: String,
    /// Matched anywhere in the text (an email, a path), not only as a word.
    anywhere: bool,
}

impl Terms {
    /// One term a line. White space around a term is dropped, and an empty
    /// line is no term.
    pub(crate) fn parse(text: &str) -> Self {
        let terms = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| Term {
                lowered: fold(line),
                anywhere: line.contains(['@', '/']),
            })
            .collect();
        Self { terms }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether any term is in `text`.
    pub(crate) fn matches(&self, text: &str) -> bool {
        if self.terms.is_empty() || text.is_empty() {
            return false;
        }
        let lowered = fold(text);
        self.terms.iter().any(|term| term.is_in(&lowered))
    }
}

/// Text as it is matched: lowered, and every run of white space, line
/// breaks included, one space. A name that a commit message or a paragraph
/// wraps between its two words is still the name.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Term {
    fn is_in(&self, lowered: &str) -> bool {
        if self.lowered.is_empty() {
            return false;
        }
        // Every place the term starts, overlapping ones included: a place
        // that is not a whole word must not hide one that starts inside it.
        let mut from = 0;
        while let Some(found) = lowered[from..].find(&self.lowered) {
            let at = from + found;
            if self.anywhere {
                return true;
            }
            // A word: what is on either side is not part of a word.
            let before = lowered[..at].chars().next_back();
            let after = lowered[at + self.lowered.len()..].chars().next();
            if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
                return true;
            }
            let step = lowered[at..].chars().next().map_or(1, char::len_utf8);
            from = at + step;
        }
        false
    }
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Where a term was found. Its text is never kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Finding {
    /// A line the diff adds: the file's path, unless the path itself
    /// matches, and the line's number in the new file.
    Line { path: Option<String>, line: usize },
    /// The path of a file the change adds or changes, which is not shown:
    /// its number, counted from 1, in what `git diff --name-only
    /// --no-renames --diff-filter=d` prints for the range.
    Path { file: usize },
    /// A field of a commit.
    Commit { commit: String, field: &'static str },
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Finding::Line {
                path: Some(path),
                line,
            } => write!(f, "{path}:{line} matches a protected term"),
            Finding::Line { path: None, line } => write!(
                f,
                "line {line} of a file whose path matches a protected term matches one too"
            ),
            Finding::Path { file } => write!(
                f,
                "the path of file {file} of `git diff --name-only --no-renames --diff-filter=d` \
                 for this range matches a protected term"
            ),
            Finding::Commit { commit, field } => {
                write!(f, "commit {commit}: its {field} matches a protected term")
            }
        }
    }
}

/// The paths a change adds or changes that match: `names` is what `git diff
/// --name-only -z` printed. A file that is only deleted is not listed.
pub(crate) fn scan_paths(terms: &Terms, names: &str) -> Vec<Finding> {
    names
        .split('\0')
        .filter(|name| !name.is_empty())
        .enumerate()
        .filter(|(_, name)| terms.matches(name))
        .map(|(index, _)| Finding::Path { file: index + 1 })
        .collect()
}

/// What a diff made with `--unified=0` adds that matches: its lines. A
/// deleted line is not looked at: taking a term out is what is wanted.
/// Paths are [`scan_paths`]'s: a file with no lines, or whose mode alone
/// changed, has no `+++` line here.
pub(crate) fn scan_diff(terms: &Terms, diff: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    // The current file's path, and whether it matched.
    let mut path: Option<(String, bool)> = None;
    let mut line = 0usize;
    // Between a file's `diff --git` line and its first hunk: where `+++` is
    // the new path. In a hunk it is an added line that begins with `++`.
    let mut in_header = false;
    for text in diff.lines() {
        if text.starts_with("diff --git ") {
            path = None;
            in_header = true;
        } else if let Some(new) = text.strip_prefix("+++ ").filter(|_| in_header) {
            // `+++ /dev/null` is a deletion: there is no new path.
            let new = new.strip_prefix("b/").unwrap_or(new).to_owned();
            let matched = new != "/dev/null" && terms.matches(&new);
            path = Some((new, matched));
        } else if let Some(hunk) = text.strip_prefix("@@ ").filter(|_| path.is_some()) {
            in_header = false;
            line = new_start(hunk).unwrap_or(0);
        } else if let Some(added) = text.strip_prefix('+').filter(|_| !in_header) {
            if terms.matches(added) {
                let path = path.as_ref().and_then(|(path, matched)| {
                    // A path that matches is not printed beside its line.
                    (!matched).then(|| path.clone())
                });
                findings.push(Finding::Line { path, line });
            }
            line += 1;
        }
    }
    findings
}

/// The first line of the new side of a hunk: the `c` of `-a,b +c,d @@`.
fn new_start(hunk: &str) -> Option<usize> {
    let new = hunk.split_whitespace().find(|part| part.starts_with('+'))?;
    new[1..].split(',').next()?.parse().ok()
}

/// The fields of the commits that `git log` printed with [`LOG_FORMAT`].
const LOG_FORMAT: &str = "%H%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%B%x1e";

/// What in a list of commits matches.
pub(crate) fn scan_commits(terms: &Terms, log: &str) -> Vec<Finding> {
    const FIELDS: [&str; 5] = [
        "author's name",
        "author's email",
        "committer's name",
        "committer's email",
        "message",
    ];
    let mut findings = Vec::new();
    let mut last: Option<String> = None;
    for record in log.split('\u{1e}') {
        let record = record.trim_start_matches(['\n', '\r']);
        if record.is_empty() {
            continue;
        }
        let mut parts = record.split('\u{1f}');
        let hash = parts.next().unwrap_or_default();
        if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            // A message that holds the separator itself: this is the rest of
            // the last commit's message, and nothing of it is printed.
            if let Some(commit) = &last
                && terms.matches(record)
            {
                findings.push(Finding::Commit {
                    commit: commit.clone(),
                    field: "message",
                });
            }
            continue;
        }
        let short: String = hash.chars().take(12).collect();
        for (field, value) in FIELDS.iter().zip(parts) {
            if terms.matches(value) {
                findings.push(Finding::Commit {
                    commit: short.clone(),
                    field,
                });
            }
        }
        last = Some(short);
    }
    findings
}

/// What `--base` and `--head` asked for.
#[derive(Debug, PartialEq)]
struct Range {
    base: String,
    head: String,
}

impl Range {
    fn parse(arguments: &[String]) -> Result<Self> {
        let (mut base, mut head) = (None, None);
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("`{argument}` needs a revision\n\n{USAGE}"))
            };
            match argument.as_str() {
                "--base" => base = Some(value()?),
                "--head" => head = Some(value()?),
                _ => return Err(format!("unknown argument `{argument}`\n\n{USAGE}")),
            }
        }
        match (base, head) {
            (Some(base), Some(head)) => Ok(Self { base, head }),
            _ => Err(format!("say both `--base` and `--head`\n\n{USAGE}")),
        }
    }

    /// The base of a first push is forty zeros: there is nothing before it.
    fn is_first_push(&self) -> bool {
        self.base.is_empty() || self.base.bytes().all(|byte| byte == b'0')
    }
}

/// `cargo xtask owner-guard`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let range = Range::parse(arguments)?;
    let terms = Terms::parse(&env::var(TERMS).unwrap_or_default());
    if terms.is_empty() {
        let message = format!(
            "{TERMS} is not set here, so nothing was checked for the owner's personal details. \
             A pull request from a fork is not given the repository's secrets; on any other run, \
             the owner has not set the secret yet."
        );
        if env::var_os("GITHUB_ACTIONS").is_some() {
            println!("::warning title=Owner guard not run::{message}");
        } else {
            println!("warning: {message}");
        }
        steps::append_job_summary(&format!("### Owner guard\n\n**Not run.** {message}\n"));
        return Ok(());
    }

    let (findings, commits) = look(&std::path::PathBuf::from("."), &range, &terms)?;
    if findings.is_empty() {
        println!("owner guard: no protected term in the diff or in its {commits} commit(s)");
        return Ok(());
    }
    let in_actions = env::var_os("GITHUB_ACTIONS").is_some();
    for finding in &findings {
        if in_actions {
            println!("::error title=Owner guard::{finding}");
        } else {
            println!("{finding}");
        }
    }
    steps::append_job_summary(&format!(
        "### Owner guard\n\n{} place(s) match a protected term:\n\n{}\n",
        findings.len(),
        findings
            .iter()
            .map(|finding| format!("- {finding}\n"))
            .collect::<String>()
    ));
    Err(format!(
        "{} place(s) match a protected term: the owner's personal details (rule 3). Take them \
         out, and rewrite the commits that carry them.",
        findings.len()
    ))
}

/// What in the repository at `dir` the range adds that matches, and how
/// many commits it has.
fn look(dir: &Path, range: &Range, terms: &Terms) -> Result<(Vec<Finding>, usize)> {
    let git = |arguments: &[&str]| git(dir, arguments);
    let has_base = !range.is_first_push()
        && git(&["cat-file", "-e", &format!("{}^{{commit}}", range.base)]).is_ok();
    if !range.is_first_push() && !has_base {
        // A force push leaves its old head on no branch, and the fetch does
        // not bring it.
        println!(
            "owner guard: the base {} is not in this repository (a force push?): the head \
             commit alone is checked",
            range.base
        );
    }
    let (diff_from, commits) = if !has_base {
        // The head commit alone, against its parent or against nothing.
        let parent = format!("{}^", range.head);
        let base = if git(&["rev-parse", "--verify", "--quiet", &parent]).is_ok() {
            parent
        } else {
            EMPTY_TREE.to_owned()
        };
        (base, vec!["-1".to_owned(), range.head.clone()])
    } else {
        // What the head adds since it left the base: three dots for the
        // diff, two for the commits.
        (
            git(&["merge-base", &range.base, &range.head])?
                .trim()
                .to_owned(),
            vec![format!("{}..{}", range.base, range.head)],
        )
    };
    let diff = git(&[
        "-c",
        "core.quotepath=off",
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-renames",
        "--text",
        "--unified=0",
        &diff_from,
        &range.head,
    ])?;
    let mut log_arguments = vec!["log".to_owned(), format!("--format={LOG_FORMAT}")];
    log_arguments.extend(commits);
    let log_arguments: Vec<&str> = log_arguments.iter().map(String::as_str).collect();
    let log = git(&log_arguments)?;

    let names = git(&[
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        "--diff-filter=d",
        &diff_from,
        &range.head,
    ])?;
    let mut findings = scan_paths(terms, &names);
    findings.extend(scan_diff(terms, &diff));
    findings.extend(scan_commits(terms, &log));
    let commits = log.split('\u{1e}').filter(|r| !r.trim().is_empty()).count();
    Ok((findings, commits))
}

/// Runs git in `dir` and returns what it printed, with any bytes that are
/// not UTF-8 replaced: a binary file's diff is text that matches no term.
/// Its error output is passed on: git prints revisions and paths there,
/// never the terms. A repository named by the environment is not used.
fn git(dir: &Path, arguments: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    for variable in GIT_LOCATION {
        command.env_remove(variable);
    }
    let output = command
        .args(arguments)
        .current_dir(dir)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            arguments.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The variables that point git at another repository than the directory's:
/// a hook exports them.
const GIT_LOCATION: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
];

#[cfg(test)]
mod tests {
    use super::{Finding, GIT_LOCATION, Range, Terms, look, scan_commits, scan_diff, scan_paths};

    /// Made-up terms of the kinds the secret holds: a name, a phrase, an
    /// email and a home path.
    const TERMS: &str = "  Quentin\n\nExample Person\r\nq.example@example.invalid\n/srv/qexample\n";

    fn terms() -> Terms {
        Terms::parse(TERMS)
    }

    #[test]
    fn a_plain_term_is_a_whole_word_in_any_case() {
        let terms = terms();
        assert!(terms.matches("Signed by quentin."));
        assert!(terms.matches("QUENTIN"));
        assert!(terms.matches("(Quentin)"));
        assert!(terms.matches("Quentin's decision"));
        // Part of a longer word is not the word.
        assert!(!terms.matches("Quentina"));
        assert!(!terms.matches("aquentin"));
        assert!(!terms.matches("quentin_rules"));
        assert!(!terms.matches("quentin2"));
        // A later occurrence that is a word is found after one that is not.
        assert!(terms.matches("Quentinx and Quentin"));
        // An occurrence that starts inside one that is not a word.
        let overlapping = Terms::parse("x x");
        assert!(overlapping.matches("yx x x"));
        assert!(!overlapping.matches("yx xy"));
    }

    #[test]
    fn a_phrase_is_matched_as_words_too() {
        let terms = terms();
        assert!(terms.matches("by example PERSON, today"));
        assert!(!terms.matches("an example personal note"));
        // White space of any kind and length between its words, as a
        // wrapped line has.
        assert!(terms.matches("example  person"));
        assert!(terms.matches("by Example\n  Person."));
        assert!(terms.matches("Example\tPerson"));
    }

    #[test]
    fn an_email_or_a_path_is_matched_anywhere() {
        let terms = terms();
        assert!(terms.matches("Q.Example@Example.Invalid"));
        assert!(terms.matches("<q.example@example.invalid>"));
        assert!(terms.matches("mailto:xq.example@example.invalidx"));
        assert!(terms.matches("cd /SRV/qexample/Coding"));
        assert!(terms.matches("/srv/qexamples"));
        assert!(!terms.matches("/srv/other"));
    }

    #[test]
    fn white_space_around_a_term_and_empty_lines_are_not_terms() {
        let terms = terms();
        assert_eq!(terms.terms.len(), 4);
        assert!(!terms.matches(""));
        assert!(!terms.matches("   "));
        assert!(Terms::parse("\n \r\n").is_empty());
        assert!(!Terms::parse("").matches("anything"));
    }

    #[test]
    fn letters_beyond_ascii_are_folded_and_bound_words() {
        let terms = Terms::parse("Zoë");
        assert!(terms.matches("by ZOË."));
        assert!(!terms.matches("Zoëy"));
        assert!(!terms.matches("éZoë"));
    }

    const DIFF: &str = "\
diff --git a/docs/notes.md b/docs/notes.md
index 1111111..2222222 100644
--- a/docs/notes.md
+++ b/docs/notes.md
@@ -3,0 +4,2 @@ heading
+A plain line.
+Written by Quentin.
@@ -10 +12 @@
-Removed: Quentin was here.
+Kept.
diff --git a/src/quentin/lib.rs b/src/quentin/lib.rs
new file mode 100644
--- /dev/null
+++ b/src/quentin/lib.rs
@@ -0,0 +1,2 @@
+// q.example@example.invalid
+fn main() {}
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-Quentin
";

    #[test]
    fn a_diff_is_read_for_its_added_lines() {
        let findings = scan_diff(&terms(), DIFF);
        assert_eq!(
            findings,
            [
                Finding::Line {
                    path: Some("docs/notes.md".to_owned()),
                    line: 5
                },
                // A path that matches is not printed beside its line.
                Finding::Line {
                    path: None,
                    line: 1
                },
            ]
        );
        // What is printed has the path and the line, and no term.
        assert_eq!(
            findings[0].to_string(),
            "docs/notes.md:5 matches a protected term"
        );
        for finding in &findings {
            assert!(!terms().matches(&finding.to_string()), "{finding}");
        }
    }

    #[test]
    fn an_added_line_that_looks_like_a_header_is_still_a_line() {
        let diff = "\
diff --git a/a.md b/a.md
--- a/a.md
+++ b/a.md
@@ -1,0 +2,2 @@
+++ Quentin
+@@ also Quentin
";
        assert_eq!(
            scan_diff(&terms(), diff),
            [
                Finding::Line {
                    path: Some("a.md".to_owned()),
                    line: 2
                },
                Finding::Line {
                    path: Some("a.md".to_owned()),
                    line: 3
                },
            ]
        );
    }

    #[test]
    fn every_path_the_change_adds_or_changes_is_read() {
        // As `git diff --name-only -z` prints them: an empty new file, a
        // path with a space and one with a tab are paths like any other.
        let names = "docs/notes.md\0fixtures/Quentin/.gitkeep\0a b.txt\0dir\tQuentin\0";
        assert_eq!(
            scan_paths(&terms(), names),
            [Finding::Path { file: 2 }, Finding::Path { file: 4 }]
        );
        assert_eq!(scan_paths(&terms(), ""), []);
        let printed = Finding::Path { file: 2 }.to_string();
        assert!(printed.ends_with("matches a protected term"), "{printed}");
        assert!(!terms().matches(&printed));
    }

    #[test]
    fn a_message_that_holds_the_separators_prints_nothing_of_itself() {
        let hash = "e".repeat(40);
        let log = format!(
            "{hash}\u{1f}A\u{1f}a@x.invalid\u{1f}A\u{1f}a@x.invalid\u{1f}Start\u{1e}Quentin \
             here\u{1f}::error::x\u{1e}\n"
        );
        assert_eq!(
            scan_commits(&terms(), &log),
            [Finding::Commit {
                commit: "e".repeat(12),
                field: "message"
            }]
        );
    }

    #[test]
    fn a_diff_with_nothing_protected_finds_nothing() {
        assert_eq!(scan_diff(&Terms::parse("Nobody"), DIFF), []);
    }

    fn log(records: &[[&str; 6]]) -> String {
        records
            .iter()
            .map(|fields| format!("{}\u{1e}\n", fields.join("\u{1f}")))
            .collect()
    }

    #[test]
    fn a_commit_is_read_for_its_people_and_its_message() {
        let log = log(&[
            [
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "Cortex-Harness",
                "dev@cortex-harness.invalid",
                "Cortex-Harness",
                "dev@cortex-harness.invalid",
                "Do a thing\n\nAnd say why.\n",
            ],
            [
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "Quentin Example",
                "Q.Example@example.invalid",
                "GitHub",
                "noreply@github.com",
                "Fix it\n\nCo-authored-by: someone <x@example.invalid>\n",
            ],
            [
                "cccccccccccccccccccccccccccccccccccccccc",
                "Cortex-Harness",
                "dev@cortex-harness.invalid",
                "Cortex-Harness",
                "dev@cortex-harness.invalid",
                "As Quentin asked\n",
            ],
        ]);
        let findings = scan_commits(&terms(), &log);
        let commit = |hash: &str, field| Finding::Commit {
            commit: hash.to_owned(),
            field,
        };
        assert_eq!(
            findings,
            [
                commit("bbbbbbbbbbbb", "author's name"),
                commit("bbbbbbbbbbbb", "author's email"),
                commit("cccccccccccc", "message"),
            ]
        );
        assert_eq!(
            findings[0].to_string(),
            "commit bbbbbbbbbbbb: its author's name matches a protected term"
        );
    }

    #[test]
    fn a_committer_is_read_too() {
        let log = log(&[[
            "dddddddddddddddddddddddddddddddddddddddd",
            "Cortex-Harness",
            "dev@cortex-harness.invalid",
            "Quentin",
            "someone@example.invalid",
            "Message\n",
        ]]);
        assert_eq!(
            scan_commits(&terms(), &log),
            [Finding::Commit {
                commit: "dddddddddddd".to_owned(),
                field: "committer's name"
            }]
        );
    }

    #[test]
    fn the_range_needs_a_base_and_a_head_and_knows_a_first_push() {
        let arguments = |list: &[&str]| -> Vec<String> {
            list.iter().map(|argument| (*argument).to_owned()).collect()
        };
        let range = Range::parse(&arguments(&["--base", "abc", "--head", "def"])).unwrap();
        assert_eq!(range.base, "abc");
        assert!(!range.is_first_push());
        assert!(Range::parse(&arguments(&["--base", "abc"])).is_err());
        assert!(Range::parse(&arguments(&["--head"])).is_err());
        assert!(Range::parse(&arguments(&["--other", "x"])).is_err());
        let first = Range::parse(&arguments(&["--base", &"0".repeat(40), "--head", "def"]));
        assert!(first.unwrap().is_first_push());
        let empty = Range::parse(&arguments(&["--base", "", "--head", "def"]));
        assert!(empty.unwrap().is_first_push());
    }

    /// A real repository: the guard runs git on it as CI does.
    #[test]
    fn sys_a_real_range_is_read_for_its_lines_its_paths_and_its_people() {
        let dir = std::env::temp_dir().join(format!("xtask-owner-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The machine's git configuration is not read: an empty file of the
        // test's own, beside the repository and not in it.
        let config = dir.with_extension("gitconfig");
        std::fs::write(&config, "").unwrap();
        let git = |arguments: &[&str], who: &str| {
            let mut command = std::process::Command::new("git");
            for variable in GIT_LOCATION {
                command.env_remove(variable);
            }
            for (variable, value) in [
                ("GIT_AUTHOR_NAME", who),
                ("GIT_COMMITTER_NAME", who),
                ("GIT_AUTHOR_EMAIL", "x@example.invalid"),
                ("GIT_COMMITTER_EMAIL", "x@example.invalid"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ] {
                command.env(variable, value);
            }
            let status = command
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(arguments)
                .current_dir(&dir)
                .env("GIT_CONFIG_GLOBAL", &config)
                .output()
                .unwrap();
            assert!(status.status.success(), "{status:?}");
            String::from_utf8(status.stdout).unwrap()
        };
        git(&["init", "-q"], "Someone");
        std::fs::write(dir.join("a.txt"), "Quentin was here before\n").unwrap();
        git(&["add", "."], "Someone");
        git(&["commit", "-q", "-m", "First"], "Someone");
        let base = git(&["rev-parse", "HEAD"], "Someone").trim().to_owned();
        std::fs::write(
            dir.join("a.txt"),
            "Quentin was here before\nclean\nby quentin\n",
        )
        .unwrap();
        git(&["add", "."], "Someone");
        git(&["commit", "-q", "-m", "Second"], "Quentin Example");
        let head = git(&["rev-parse", "HEAD"], "Someone").trim().to_owned();

        let range = Range {
            base: base.clone(),
            head: head.clone(),
        };
        let (findings, commits) = look(&dir, &range, &terms()).unwrap();
        assert_eq!(commits, 1);
        assert_eq!(
            findings,
            [
                // The line the range adds, and not the one it kept.
                Finding::Line {
                    path: Some("a.txt".to_owned()),
                    line: 3
                },
                Finding::Commit {
                    commit: head[..12].to_owned(),
                    field: "author's name"
                },
                Finding::Commit {
                    commit: head[..12].to_owned(),
                    field: "committer's name"
                },
            ]
        );

        // A first push is the head commit alone, against its parent.
        let first = Range {
            base: "0".repeat(40),
            head: base.clone(),
        };
        let (findings, commits) = look(&dir, &first, &terms()).unwrap();
        assert_eq!(commits, 1);
        assert_eq!(
            findings,
            [Finding::Line {
                path: Some("a.txt".to_owned()),
                line: 1
            }]
        );

        // A base this repository does not have, as after a force push: the
        // head commit alone.
        let gone = Range {
            base: "1".repeat(40),
            head: head.clone(),
        };
        let (findings, commits) = look(&dir, &gone, &terms()).unwrap();
        assert_eq!(commits, 1);
        assert_eq!(findings.len(), 3, "{findings:?}");

        // A binary file and a new empty file whose path matches.
        std::fs::write(dir.join("logo.png"), [0x89u8, 0xff, 0xfe, 0x00, 0x80]).unwrap();
        std::fs::create_dir_all(dir.join("quentin")).unwrap();
        std::fs::write(dir.join("quentin/.gitkeep"), "").unwrap();
        git(&["add", "."], "Someone");
        git(&["commit", "-q", "-m", "Third"], "Someone");
        let third = git(&["rev-parse", "HEAD"], "Someone").trim().to_owned();
        let range = Range {
            base: head,
            head: third,
        };
        let (findings, _) = look(&dir, &range, &terms()).unwrap();
        assert_eq!(findings, [Finding::Path { file: 2 }]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&config);
    }
}
