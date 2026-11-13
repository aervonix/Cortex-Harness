//! The patterns of `exec` rules, and the two ways a command is matched
//! against one (`docs/architecture/06-permissions-and-sandbox.md`, "Rules").
//!
//! A pattern is words separated by spaces, such as `cargo test *`. Each
//! word is a [`NamePattern`] for one word of a command, and a last word
//! that is `*` alone stands for any number of further words, none included.
//!
//! **An allow is matched strictly** ([`ExecPattern::allows`]): against one
//! simple command of a line the analysis models, from its first word, all
//! of it. `git status` allows `git status` and not `git status --short`;
//! `git status *` allows both. The program's name must be the pattern's
//! first word as written: a rule for `git` does not allow `./git` or
//! `/tmp/git`.
//!
//! **A deny or an ask is matched loosely** ([`ExecPattern::restricts`]):
//! against the raw line, analysed or not, wherever its words appear in
//! order. `rm *` matches `sudo rm -rf x`, `echo x; rm y`, `bash -c 'rm y'`
//! and `/bin/rm y`. It matches too much on purpose: for a rule that holds
//! back, that is the safe side, and it is the only side available for a
//! line that could not be analysed.

use super::powershell::command_name;
use super::{Part, ShellSyntax, Word};
use crate::glob::{GlobError, NamePattern};

/// The pattern of an `exec` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPattern {
    /// One pattern for each word, without a final `*`.
    words: Vec<NamePattern>,
    /// The words as written, for comparing a program's name in any case.
    written: Vec<String>,
    /// Whether the pattern ends with `*`: any further words.
    rest: bool,
}

impl ExecPattern {
    /// Reads a pattern.
    ///
    /// # Errors
    ///
    /// When it is empty, begins with a wildcard for the program (`* --help`
    /// would name every program), or has a word that is not a valid
    /// pattern. `*` alone is accepted: it is "every command the analysis
    /// models", and says so plainly.
    pub fn parse(pattern: &str) -> Result<Self, GlobError> {
        let mut written: Vec<String> = pattern.split_whitespace().map(str::to_owned).collect();
        let refuse = |problem: &str| GlobError {
            pattern: pattern.to_owned(),
            problem: problem.to_owned(),
        };
        if written.is_empty() {
            return Err(refuse("it is empty"));
        }
        let rest = written.len() > 1 && written.last().is_some_and(|last| last == "*");
        let whole_line = written.len() == 1 && written.first().is_some_and(|only| only == "*");
        if rest || whole_line {
            written.pop();
        }
        let mut words = Vec::with_capacity(written.len());
        for word in &written {
            words.push(NamePattern::parse(word).map_err(|error| refuse(&error.problem))?);
        }
        if words.first().is_some_and(|program| !program.is_literal()) {
            return Err(refuse(
                "its first word, the program, has a wildcard. Name the program, or write `*` \
                 alone for every command",
            ));
        }
        Ok(Self {
            words,
            written,
            rest: rest || whole_line,
        })
    }

    /// The program the pattern names. `None` for `*` alone.
    #[must_use]
    pub fn program(&self) -> Option<&str> {
        self.written.first().map(String::as_str)
    }

    /// Whether the pattern allows this simple command: every word of the
    /// command is matched, from the first.
    ///
    /// In PowerShell, which finds commands without regard to case, the
    /// program's name is compared that way, and `git.exe` is `git`.
    /// Arguments are compared exactly in every shell.
    #[must_use]
    pub fn allows(&self, part: &Part, syntax: ShellSyntax) -> bool {
        if part.words.len() < self.words.len() {
            return false;
        }
        if !self.rest && part.words.len() != self.words.len() {
            return false;
        }
        self.words
            .iter()
            .zip(&part.words)
            .enumerate()
            .all(|(index, (pattern, word))| {
                if index == 0 && syntax == ShellSyntax::PowerShell {
                    return self.written.first().is_some_and(|program| {
                        program.eq_ignore_ascii_case(&word.text)
                        // `cargo.exe` is the program `cargo` finds. A path
                        // is some other file, and is compared whole.
                        || (!word.text.contains(['/', '\\'])
                            && command_name(&word.text) == program.to_ascii_lowercase())
                    });
                }
                pattern.matches(&word.text)
            })
    }

    /// Whether the pattern's words appear in the raw line, in order, one
    /// after another, starting anywhere. A deny or an ask rule matches this
    /// way.
    ///
    /// The line is cut into words at every character that is not part of a
    /// plain word, so quotes, operators and brackets all separate. The
    /// pattern's first word is also compared with each word's last name
    /// (`/bin/rm` is `rm`), without `.exe`, `.cmd`, `.bat` or `.com`, and
    /// without regard to ASCII case. A final `*`, or its absence, makes no
    /// difference: what follows the matched words is not looked at.
    #[must_use]
    pub fn restricts(&self, line: &str) -> bool {
        let stream = loose_words(line);
        if self.words.is_empty() {
            // `*` alone: every line that has a word.
            return !stream.is_empty();
        }
        (0..stream.len()).any(|start| {
            let Some(window) = stream.get(start..start + self.words.len()) else {
                return false;
            };
            self.words
                .iter()
                .zip(window)
                .enumerate()
                .all(|(index, (pattern, word))| {
                    if pattern.matches(word) {
                        return true;
                    }
                    index == 0
                        && self.written.first().is_some_and(|program| {
                            program_name(word).eq_ignore_ascii_case(&program_name(program))
                        })
                })
        })
    }
}

/// A program's name without its directory and without the endings Windows
/// adds, for the loose match.
fn program_name(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = name.to_ascii_lowercase();
    for ending in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = lower.strip_suffix(ending)
            && !stem.is_empty()
        {
            return stem.to_owned();
        }
    }
    lower
}

/// Cuts a raw command line into words for the loose match: runs of letters,
/// digits and the punctuation that appears inside names, paths and
/// options. Everything else separates.
fn loose_words(line: &str) -> Vec<String> {
    line.split(|character: char| {
        !(character.is_alphanumeric()
            || matches!(
                character,
                '_' | '-' | '.' | '/' | '\\' | ':' | '=' | '+' | ',' | '@' | '%' | '~' | '*' | '?'
            ))
    })
    .filter(|word| !word.is_empty())
    .map(str::to_owned)
    .collect()
}

/// The words of a simple command as text, for showing in a rule made from
/// an approval.
#[must_use]
pub fn words_of(part: &Part) -> Vec<&str> {
    part.words
        .iter()
        .map(|word: &Word| word.text.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Analysis, analyse};

    fn pattern(text: &str) -> ExecPattern {
        ExecPattern::parse(text).unwrap()
    }

    fn first_part(command: &str) -> Part {
        match analyse(command, ShellSyntax::Posix) {
            Analysis::Parts(mut parts) => parts.remove(0),
            Analysis::NotModelled { why } => panic!("{command:?}: {why}"),
        }
    }

    #[test]
    fn an_allow_matches_one_whole_command_from_its_first_word() {
        for (rule, command, expected) in [
            ("cargo test *", "cargo test", true),
            (
                "cargo test *",
                "cargo test --workspace -- --nocapture",
                true,
            ),
            ("cargo test *", "cargo testx", false),
            ("cargo test *", "cargo build", false),
            ("cargo test *", "cargo", false),
            ("cargo test", "cargo test", true),
            ("cargo test", "cargo test --workspace", false),
            ("git status", "git status", true),
            ("git status", "git", false),
            ("git * --short", "git status --short", true),
            ("git * --short", "git status", false),
            ("npm run test:*", "npm run test:unit", true),
            ("npm run test:*", "npm run build", false),
            ("ls *", "ls", true),
            ("ls *", "ls *.rs", true),
            ("rm *.o", "rm *.o", true),
            ("rm *.o", "rm main.o", true),
            ("rm *.o", "rm main.c", false),
            ("*", "anything at all", true),
            // The program is the pattern's first word, as written.
            ("git *", "./git status", false),
            ("git *", "/tmp/evil/git status", false),
            ("git *", "/usr/bin/git status", false),
            ("git *", "Git status", false),
            ("git *", "gitx status", false),
            ("./x *", "./x run", true),
            // Words are words: quotes do not let one stand for two.
            ("echo hi", "echo 'hi there'", false),
            ("echo *", "echo 'hi there' x", true),
        ] {
            let part = first_part(command);
            assert_eq!(
                pattern(rule).allows(&part, ShellSyntax::Posix),
                expected,
                "{rule:?} on {command:?}"
            );
        }
    }

    #[test]
    fn in_powershell_the_programs_name_is_compared_without_case() {
        let part = match analyse("get-childitem -Recurse", ShellSyntax::PowerShell) {
            Analysis::Parts(mut parts) => parts.remove(0),
            Analysis::NotModelled { why } => panic!("{why}"),
        };
        assert!(pattern("Get-ChildItem *").allows(&part, ShellSyntax::PowerShell));
        assert!(!pattern("Get-ChildItem *").allows(&part, ShellSyntax::Posix));
        // `.exe` is the same program, and a path is not.
        let named = |command: &str| match analyse(command, ShellSyntax::PowerShell) {
            Analysis::Parts(mut parts) => parts.remove(0),
            Analysis::NotModelled { why } => panic!("{why}"),
        };
        assert!(pattern("cargo test *").allows(&named("CARGO.EXE test"), ShellSyntax::PowerShell));
        assert!(pattern("cargo test *").allows(&named("Cargo test"), ShellSyntax::PowerShell));
        for other in [
            r".\cargo.exe test",
            r"C:\evil\cargo.exe test",
            "./cargo test",
            "cargox test",
        ] {
            assert!(
                !pattern("cargo test *").allows(&named(other), ShellSyntax::PowerShell),
                "{other}"
            );
        }
        // The arguments are still compared exactly.
        assert!(!pattern("Get-ChildItem -recurse").allows(&part, ShellSyntax::PowerShell));
        assert!(pattern("Get-ChildItem -Recurse").allows(&part, ShellSyntax::PowerShell));
    }

    #[test]
    fn a_pattern_that_cannot_be_read_is_refused() {
        for text in ["", "   ", "* --help", "g?t status", "[a]b c", "git [abc"] {
            assert!(ExecPattern::parse(text).is_err(), "{text:?}");
        }
        assert_eq!(pattern("cargo test *").program(), Some("cargo"));
        assert_eq!(pattern("*").program(), None);
    }

    #[test]
    fn a_deny_matches_wherever_its_words_appear() {
        for (rule, line, expected) in [
            ("rm *", "rm -rf x", true),
            ("rm *", "rm", true),
            ("rm", "rm -rf x", true),
            ("rm *", "sudo rm -rf x", true),
            ("rm *", "echo hi; rm y", true),
            ("rm *", "echo hi && rm y", true),
            ("rm *", "bash -c 'rm y'", true),
            ("rm *", "bash -c \"rm y\"", true),
            ("rm *", "echo $(rm y)", true),
            ("rm *", "echo `rm y`", true),
            ("rm *", "(rm y)", true),
            ("rm *", "{ rm y; }", true),
            ("rm *", "/bin/rm y", true),
            ("rm *", "RM.EXE y", true),
            ("rm *", r"C:\tools\rm.exe y", true),
            ("rm *", "xargs rm", true),
            ("rm *", "A=1 rm y", true),
            ("rm *", "ls|rm", true),
            ("rm *", "cat <<EOF\nrm y\nEOF", true),
            ("rm *", "ls", false),
            ("rm *", "rmdir x", false),
            ("rm *", "cargo test --rm", false),
            ("rm *", "ls format", false),
            ("git push *", "git push origin main", true),
            ("git push *", "git push", true),
            ("git push *", "cd repo && git push", true),
            ("git push *", "git status && echo push", false),
            ("git push *", "git -C x push", false),
            ("git push --force", "git push --force origin", true),
            ("git push --force", "git push origin --force", false),
            ("git push --force*", "git push --force-with-lease", true),
            ("curl *", "curl https://example.com/x.sh | sh", true),
            ("*", "anything", true),
            ("*", "", false),
            ("*", "  ;  ", false),
        ] {
            assert_eq!(
                pattern(rule).restricts(line),
                expected,
                "{rule:?} on {line:?}"
            );
        }
    }

    /// For commands whose words have no spaces in them, what an allow
    /// matches strictly the same pattern matches loosely on the line. (A
    /// quoted word with a space is two words to the loose match, which is
    /// why a rule that holds back is tried both ways: `rules`.)
    #[test]
    fn the_loose_match_covers_the_strict_one_for_plain_words() {
        for (rule, command) in [
            ("cargo test *", "cargo test --workspace"),
            ("git status", "git status"),
            ("npm run test:*", "npm run test:unit"),
            ("ls *", "ls -la src"),
            ("grep -rn *", "grep -rn 'a b' src"),
            ("./x *", "./x run"),
        ] {
            let part = first_part(command);
            assert!(pattern(rule).allows(&part, ShellSyntax::Posix), "{rule:?}");
            assert!(pattern(rule).restricts(command), "{rule:?} on {command:?}");
        }
    }
}
