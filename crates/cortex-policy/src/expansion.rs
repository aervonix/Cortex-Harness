//! What a word the shell expands as a pattern could name
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Command analysis as
//! built").
//!
//! A word with `*`, `?` or `[…]` in it is replaced by the shell with the
//! names of the files it matches, which the analysis cannot know: `cp evil
//! .gi?/hooks/pre-commit` writes `.git/hooks/pre-commit`. So a pattern is
//! taken to name every path it could match, and asked about where any of
//! those could be one the engine protects. This module answers the one
//! question that needs: could a pattern for one name match a name, or a
//! name a pattern of the engine's own describes (`*.pem`)?
//!
//! It errs towards "yes":
//!
//! - **Names are compared without regard to case.**
//! - **A `[` matches anything from there to the end of the name.** A set
//!   has classes (`[[:alpha:]]`), ranges whose ends a fold of case would
//!   move (`[U-z]`), and members that were quoted (`["!"t]`, `[a"]"t]`),
//!   which the word's text no longer marks. Reading a set exactly would be
//!   wrong in each of those; reading it as "anything" never is (found by
//!   the second independent review of the fixes to step 12).
//! - **A name longer than any file's** (255 bytes) could be anything.
//!
//! The one rule of the shells it keeps is that a name that begins with `.`
//! is matched only by a pattern that begins with a `.` written out (or a
//! `[`): `*` and `?` at the start of a name never match a hidden file in
//! bash, dash, zsh or ksh, unless an option the user sets says otherwise
//! (`dotglob`, `GLOB_DOTS`). That is what lets `rm *.o` run while `rm .e*`
//! is asked about.

use std::collections::HashSet;

/// The longest name a file system gives a file, in bytes. A pattern for a
/// longer name is taken to match anything, which also bounds the search.
const LONGEST_NAME: usize = 255;

/// One part of a pattern for one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// That character.
    Char(char),
    /// Any one character: `?`.
    Any,
    /// Any run of characters, none included: `*`, or a `[` and everything
    /// after it.
    Star,
}

/// A pattern for one name, as a shell reads one in a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamePattern {
    tokens: Vec<Token>,
    /// Whether the pattern begins with a `[`, which may hold a `.`.
    opens_with_set: bool,
    /// Whether the text was longer than any name.
    too_long: bool,
}

impl NamePattern {
    /// Reads a pattern for one name. Every text is a pattern: one with no
    /// `*`, `?` or `[` matches itself.
    pub(crate) fn parse(text: &str) -> Self {
        let mut tokens = Vec::new();
        let mut opens_with_set = false;
        for character in text.chars().flat_map(char::to_lowercase) {
            match character {
                '*' => {
                    if tokens.last() != Some(&Token::Star) {
                        tokens.push(Token::Star);
                    }
                }
                '?' => tokens.push(Token::Any),
                '[' => {
                    opens_with_set = tokens.is_empty();
                    if tokens.last() != Some(&Token::Star) {
                        tokens.push(Token::Star);
                    }
                    break;
                }
                other => tokens.push(Token::Char(other)),
            }
        }
        Self {
            tokens,
            opens_with_set,
            too_long: text.len() > LONGEST_NAME,
        }
    }

    /// Whether some one name matches both this pattern and `other`. With
    /// `hidden_rule`, a name that begins with `.` is matched by this
    /// pattern only where it begins with a `.` written out or a `[`, as a
    /// shell's pattern does.
    pub(crate) fn could_match(&self, other: &Self, hidden_rule: bool) -> bool {
        if self.too_long || other.too_long {
            return true;
        }
        if hidden_rule
            && !self.opens_with_set
            && matches!(self.tokens.first(), Some(Token::Star | Token::Any))
            && other.tokens.first() == Some(&Token::Char('.'))
        {
            return false;
        }
        meet(&self.tokens, &other.tokens)
    }

    /// Whether this pattern could match the name `name`, a name and not a
    /// pattern.
    pub(crate) fn could_match_name(&self, name: &str, hidden_rule: bool) -> bool {
        let name = Self {
            tokens: name
                .chars()
                .flat_map(char::to_lowercase)
                .map(Token::Char)
                .collect(),
            opens_with_set: false,
            too_long: name.len() > LONGEST_NAME,
        };
        self.could_match(&name, hidden_rule)
    }
}

/// Whether a text holds a character a shell's pattern reads.
pub(crate) fn has_wildcards(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// Whether the two patterns could match one text: a search over pairs of
/// positions, each visited once, kept on a list of its own rather than the
/// stack, so that a long pattern cannot exhaust it.
fn meet(first: &[Token], second: &[Token]) -> bool {
    let mut seen = HashSet::new();
    let mut to_visit = vec![(0, 0)];
    while let Some((i, j)) = to_visit.pop() {
        if i == first.len() && j == second.len() {
            return true;
        }
        if !seen.insert((i, j)) {
            continue;
        }
        let one = first.get(i).copied();
        let two = second.get(j).copied();
        // A `*` matches nothing more, or one more character the other part
        // matches.
        if one == Some(Token::Star) {
            to_visit.push((i + 1, j));
            if two.is_some() {
                to_visit.push((i, j + 1));
            }
        }
        if two == Some(Token::Star) {
            to_visit.push((i, j + 1));
            if one.is_some() {
                to_visit.push((i + 1, j));
            }
        }
        if let (Some(one), Some(two)) = (one, two)
            && one != Token::Star
            && two != Token::Star
            && match (one, two) {
                (Token::Char(a), Token::Char(b)) => a == b,
                _ => true,
            }
        {
            to_visit.push((i + 1, j + 1));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn could(pattern: &str, other: &str) -> bool {
        NamePattern::parse(pattern).could_match(&NamePattern::parse(other), true)
    }

    #[test]
    fn a_pattern_matches_the_names_a_shell_would_give_it() {
        for (pattern, name, expected) in [
            (".gi?", ".git", true),
            (".g*", ".git", true),
            ("[.]git", ".git", true),
            (".gi[a-z]", ".git", true),
            (".git", ".git", true),
            (".GIT", ".git", true),
            ("hook[s]", "hooks", true),
            ("*", "head", true),
            ("h*", "head", true),
            ("?ead", "head", true),
            ("*.o", "head", false),
            // A set is anything to the end of the name: classes, ranges
            // that a fold of case would move, and members that were quoted
            // are never misread. Found by the second independent review of
            // the fixes to step 12.
            (".gi[[:alpha:]]", ".git", true),
            (".gi[[=t=]]", ".git", true),
            (".gi[U-z]", ".git", true),
            ("[A-_]EAD", "HEAD", true),
            (".gi[!t]", ".git", true),
            (".gi[a]t]", ".git", true),
            (".pre[a-z]commit-config.yaml", ".pre-commit-config.yaml", true),
            ("x[", "x", true),
            // A hidden name needs a `.` written out, or a set.
            ("*", ".git", false),
            ("?git", ".git", false),
            ("*git", ".git", false),
            ("*.rs", "id_rsa", false),
            ("src", ".git", false),
        ] {
            assert_eq!(
                NamePattern::parse(pattern).could_match_name(name, true),
                expected,
                "{pattern:?} and {name:?}"
            );
        }
        // Without the shells' rule, `*` matches a hidden name too.
        assert!(NamePattern::parse("*").could_match_name(".git", false));
    }

    #[test]
    fn two_patterns_meet_where_one_name_matches_both() {
        for (pattern, other, expected) in [
            ("*.pem", "*.pem", true),
            ("server.*", "*.pem", true),
            ("*.o", "*.pem", false),
            ("*.o", ".env.*", false),
            (".e*", ".env.*", true),
            (".e*", ".env", true),
            ("*", "*.key", true),
            ("a?c", "*b*", true),
            ("a?c", "*d*", true),
            ("a?c", "*dd*", false),
            ("*.[pP]em", "*.pem", true),
            ("*", ".env", false),
            ("*x*y*", "*y*x*", true),
        ] {
            assert_eq!(could(pattern, other), expected, "{pattern:?} and {other:?}");
        }
    }

    #[test]
    fn a_very_long_pattern_is_anything_and_does_not_exhaust_the_stack() {
        let long = format!("{}*", "a".repeat(30_000));
        assert!(could(&long, "*.pem"));
        let deep = format!("{}*.o", "a".repeat(200));
        assert!(!could(&deep, "*.pem"));
        assert!(could(&format!("{}*", "a".repeat(200)), "*.pem"));
    }

    #[test]
    fn has_wildcards_finds_what_a_shell_reads() {
        assert!(has_wildcards("*.rs"));
        assert!(has_wildcards("a?"));
        assert!(has_wildcards("[ab]"));
        assert!(!has_wildcards("src/main.rs"));
    }
}
