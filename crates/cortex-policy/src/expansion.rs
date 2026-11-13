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
//! It errs towards "yes": a set (`[…]`) is taken to hold any character
//! where two sets meet, and names are compared without regard to case.
//! The one rule of the shells it keeps is that a name that begins with `.`
//! is matched only by a pattern that begins with a `.` written out (or a
//! set): `*` and `?` at the start of a name never match a hidden file in
//! bash, dash, zsh or ksh, unless an option the user sets says otherwise.
//! That is what lets `rm *.o` run while `rm .e*` is asked about.

/// One part of a pattern for one name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// That character.
    Char(char),
    /// Any one character: `?`.
    Any,
    /// One character of a set, or of its complement: `[a-z]`, `[!.]`.
    Set {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    /// Any run of characters, none included: `*`.
    Star,
}

impl Token {
    fn is_single(&self) -> bool {
        !matches!(self, Self::Star)
    }

    /// Whether the set could hold `character`.
    fn admits(&self, character: char) -> bool {
        match self {
            Self::Char(own) => *own == character,
            Self::Any | Self::Star => true,
            Self::Set { ranges, negated } => {
                let inside = ranges
                    .iter()
                    .any(|(low, high)| (*low..=*high).contains(&character));
                inside != *negated
            }
        }
    }

    /// Whether two single-character parts could match one character.
    fn meets(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Char(character), other) | (other, Self::Char(character)) => {
                other.admits(*character)
            }
            // Two sets, `?`, or `*`: some character could well be in both.
            _ => true,
        }
    }
}

/// A pattern for one name, as a shell reads one in a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamePattern {
    tokens: Vec<Token>,
}

impl NamePattern {
    /// Reads a pattern for one name. Every text is a pattern: one with no
    /// `*`, `?` or `[` matches itself. A `[` with no `]` after it is the
    /// character. Letters are taken in lower case.
    pub(crate) fn parse(text: &str) -> Self {
        let chars: Vec<char> = text.to_lowercase().chars().collect();
        let mut tokens = Vec::new();
        let mut at = 0;
        while let Some(&character) = chars.get(at) {
            at += 1;
            match character {
                '*' => {
                    if tokens.last() != Some(&Token::Star) {
                        tokens.push(Token::Star);
                    }
                }
                '?' => tokens.push(Token::Any),
                '[' => match set(&chars, at) {
                    Some((token, next)) => {
                        tokens.push(token);
                        at = next;
                    }
                    None => tokens.push(Token::Char('[')),
                },
                other => tokens.push(Token::Char(other)),
            }
        }
        Self { tokens }
    }

    /// Whether some one name matches both this pattern and `other`. With
    /// `hidden_rule`, a name that begins with `.` is matched by this
    /// pattern only where it begins with a `.` written out or a set, as a
    /// shell's pattern does.
    pub(crate) fn could_match(&self, other: &Self, hidden_rule: bool) -> bool {
        if hidden_rule
            && matches!(self.tokens.first(), Some(Token::Star | Token::Any))
            && other.tokens.first() == Some(&Token::Char('.'))
        {
            return false;
        }
        let mut seen = std::collections::HashSet::new();
        meet(&self.tokens, &other.tokens, 0, 0, &mut seen)
    }

    /// Whether this pattern could match the name `name`, a name and not a
    /// pattern.
    pub(crate) fn could_match_name(&self, name: &str, hidden_rule: bool) -> bool {
        let name = Self {
            tokens: name.to_lowercase().chars().map(Token::Char).collect(),
        };
        self.could_match(&name, hidden_rule)
    }
}

/// Whether a text holds a character a shell's pattern reads.
pub(crate) fn has_wildcards(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// Reads a set, after its `[`: the token, and where the pattern goes on.
/// `None` when there is no `]` to close it.
fn set(chars: &[char], start: usize) -> Option<(Token, usize)> {
    let mut at = start;
    let negated = matches!(chars.get(at), Some('!' | '^'));
    if negated {
        at += 1;
    }
    let mut ranges = Vec::new();
    let mut first = true;
    loop {
        let character = *chars.get(at)?;
        if character == ']' && !first {
            return Some((Token::Set { ranges, negated }, at + 1));
        }
        first = false;
        if chars.get(at + 1) == Some(&'-') && chars.get(at + 2).is_some_and(|end| *end != ']') {
            let end = chars[at + 2];
            ranges.push((character.min(end), character.max(end)));
            at += 3;
        } else {
            ranges.push((character, character));
            at += 1;
        }
    }
}

/// Whether the two patterns, from the given positions on, could match one
/// text: a search over pairs of positions, each visited once.
fn meet(
    first: &[Token],
    second: &[Token],
    i: usize,
    j: usize,
    seen: &mut std::collections::HashSet<(usize, usize)>,
) -> bool {
    if i == first.len() && j == second.len() {
        return true;
    }
    if !seen.insert((i, j)) {
        return false;
    }
    let one = first.get(i);
    let two = second.get(j);
    // A `*` matches nothing more, or one more character the other part
    // matches.
    if one == Some(&Token::Star) {
        if meet(first, second, i + 1, j, seen) {
            return true;
        }
        if two.is_some() && meet(first, second, i, j + 1, seen) {
            return true;
        }
    }
    if two == Some(&Token::Star) {
        if meet(first, second, i, j + 1, seen) {
            return true;
        }
        if one.is_some() && meet(first, second, i + 1, j, seen) {
            return true;
        }
    }
    match (one, two) {
        (Some(one), Some(two)) if one.is_single() && two.is_single() && one.meets(two) => {
            meet(first, second, i + 1, j + 1, seen)
        }
        _ => false,
    }
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
            (".gi[!t]", ".git", false),
            (".git", ".git", true),
            (".GIT", ".git", true),
            ("hook[s]", "hooks", true),
            ("*", "head", true),
            ("h*", "head", true),
            ("?ead", "head", true),
            ("*.o", "head", false),
            ("x[", "x[", true),
            // A hidden name needs a `.` written out.
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
            ("[a-c]x", "bx", true),
            ("[a-c]x", "dx", false),
            ("*.[pP]em", "*.pem", true),
            ("*", ".env", false),
            ("*x*y*", "*y*x*", true),
        ] {
            assert_eq!(could(pattern, other), expected, "{pattern:?} and {other:?}");
        }
    }

    #[test]
    fn has_wildcards_finds_what_a_shell_reads() {
        assert!(has_wildcards("*.rs"));
        assert!(has_wildcards("a?"));
        assert!(has_wildcards("[ab]"));
        assert!(!has_wildcards("src/main.rs"));
    }
}
