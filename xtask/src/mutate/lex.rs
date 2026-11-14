//! Finding the code a break changes, in a form that survives `rustfmt`.
//!
//! A break that says "replace this exact text" stops applying the day the
//! formatter moves a line break, and a driver that then runs the tests
//! anyway reports a break as survived that was never made. So a break's
//! `find` is compared with the source **token by token**: white space and
//! comments are not tokens, and neither is a comma that only trails (one
//! right before a closing bracket), which the formatter adds and removes as
//! it wraps and joins lines.
//!
//! A break may also say which item its code is in (`in: Round::spawn`), so
//! that a short `find` need only be unique inside that function. An item is
//! found by its keyword and name, wherever the formatter put it.
//!
//! This is not a Rust parser. It knows where a token starts and ends (so a
//! brace inside a string or a comment is not a brace) and how brackets nest,
//! and that is all it needs.

use std::ops::Range;

/// One token of a source file: where it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl Token {
    /// The token's text in `source`.
    pub(crate) fn text(self, source: &str) -> &str {
        &source[self.start..self.end]
    }
}

/// The tokens of `source`, without white space, comments and trailing
/// commas.
///
/// Every punctuation character is a token of its own: `::` is two. That
/// makes `a::b` and `a: :b` the same to a comparison, which no break
/// depends on. What a break does depend on is that half an operator is not
/// taken for a token of its own, and [`locate`] sees to that: a match
/// never begins or ends inside an operator of two characters.
pub(crate) fn tokens(source: &str) -> Vec<Token> {
    lex(source, true)
}

/// The tokens of `source`. With `layout_commas` off, a trailing comma is a
/// token like any other.
fn lex(source: &str, layout_commas: bool) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let rest = &source[at..];
        if byte.is_ascii_whitespace() {
            at += 1;
        } else if rest.starts_with("//") {
            at += rest.find('\n').unwrap_or(rest.len());
        } else if rest.starts_with("/*") {
            at += block_comment(rest);
        } else if let Some(length) = string_like(rest) {
            tokens.push(Token {
                start: at,
                end: at + length,
            });
            at += length;
        } else if byte == b'\'' {
            let length = quote(rest);
            tokens.push(Token {
                start: at,
                end: at + length,
            });
            at += length;
        } else if byte.is_ascii_digit() {
            // After a `.`, digits are the name of a tuple's field: `t.0.1`
            // is two fields, and not the field `0.1`.
            let field = tokens
                .last()
                .is_some_and(|last| last.end == at && last.text(source) == ".");
            let length = if field {
                rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len())
            } else {
                number(rest)
            };
            tokens.push(Token {
                start: at,
                end: at + length,
            });
            at += length;
        } else if is_word_start(rest) {
            let length = word(rest);
            tokens.push(Token {
                start: at,
                end: at + length,
            });
            at += length;
        } else {
            let length = rest.chars().next().map_or(1, char::len_utf8);
            // A comma that only trails is the formatter's, not the code's.
            if layout_commas
                && matches!(byte, b')' | b']' | b'}')
                && tokens.last().is_some_and(|last| last.text(source) == ",")
            {
                tokens.pop();
            }
            tokens.push(Token {
                start: at,
                end: at + length,
            });
            at += length;
        }
    }
    tokens
}

/// The length of a block comment, which may hold others.
fn block_comment(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at..].starts_with(b"/*") {
            depth += 1;
            at += 2;
        } else if bytes[at..].starts_with(b"*/") {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += 1;
        }
    }
    rest.len()
}

/// The length of a string literal at the start of `rest`, with any of the
/// prefixes Rust has (`b`, `c`, `r`, `br`, `cr`, and `#`s for a raw one).
fn string_like(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut at = 0;
    if matches!(bytes.first(), Some(b'b' | b'c')) {
        at += 1;
    }
    let raw = bytes.get(at) == Some(&b'r');
    if raw {
        at += 1;
    }
    let mut hashes = 0;
    while raw && bytes.get(at) == Some(&b'#') {
        hashes += 1;
        at += 1;
    }
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    at += 1;
    if raw {
        let mut closing = String::from("\"");
        closing.push_str(&"#".repeat(hashes));
        return Some(
            rest[at..]
                .find(&closing)
                .map_or(rest.len(), |end| at + end + closing.len()),
        );
    }
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return Some(at + 1),
            _ => at += 1,
        }
    }
    Some(rest.len())
}

/// The length of what starts with `'`: a character literal, or a lifetime.
fn quote(rest: &str) -> usize {
    let mut chars = rest.char_indices().skip(1);
    match chars.next() {
        // An escape: a character literal, to its closing quote.
        Some((_, '\\')) => {
            let bytes = rest.as_bytes();
            let mut at = 1;
            while at < bytes.len() {
                match bytes[at] {
                    b'\\' => at += 2,
                    b'\'' => return at + 1,
                    _ => at += 1,
                }
            }
            rest.len()
        }
        Some((_, first)) => match chars.next() {
            // 'x'
            Some((at, '\'')) => at + 1,
            // 'label or 'lifetime
            _ if first.is_alphanumeric() || first == '_' => 1 + word(&rest[1..]),
            _ => 1,
        },
        None => 1,
    }
}

/// The length of a number: digits, letters and `_`, and a `.` when a digit
/// follows it (`1.5`, but not the `.` of `1..2` or `1.max(2)`).
fn number(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let more = byte.is_ascii_alphanumeric()
            || byte == b'_'
            || (byte == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit));
        if !more {
            break;
        }
        at += 1;
    }
    at
}

fn is_word_start(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
}

/// The length of an identifier or keyword, with the `r#` of a raw one.
fn word(rest: &str) -> usize {
    let body = rest.strip_prefix("r#").filter(|body| is_word_start(body));
    let prefix = if body.is_some() { 2 } else { 0 };
    let body = body.unwrap_or(rest);
    prefix
        + body
            .char_indices()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
            .map_or(body.len(), |(at, _)| at)
}

/// Why a break's code could not be found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    /// `find` has no tokens: it is empty, or only a comment.
    NothingToFind,
    /// No item has this name. The segment of `in` that was not found, and
    /// the path up to it.
    NoSuchItem { segment: String, within: String },
    /// `find` is not there.
    NotFound,
    /// `find` is there more than once, at these lines, and no `occurrence`
    /// says which is meant.
    Ambiguous(Vec<usize>),
    /// `occurrence` asks for a match there are not enough of.
    NoSuchOccurrence { asked: usize, found: usize },
}

/// Where `find` is in `source`: the bytes from its first token to its last.
///
/// `anchor` is the `in` of a break: item names separated by `::`. `find`
/// must be there exactly once inside the items the anchor names (the whole
/// file without one), unless `occurrence` picks one, counted from 1 in
/// source order.
#[cfg(test)]
pub(crate) fn locate(
    source: &str,
    anchor: Option<&str>,
    find: &str,
    occurrence: Option<usize>,
) -> Result<Range<usize>, Missing> {
    locate_in(source, &tokens(source), anchor, find, occurrence)
}

/// [`locate`], for a source whose tokens are already known: many breaks
/// name one file, and its tokens are worked out once.
pub(crate) fn locate_in(
    source: &str,
    all: &[Token],
    anchor: Option<&str>,
    find: &str,
    occurrence: Option<usize>,
) -> Result<Range<usize>, Missing> {
    locate_with_items(source, all, anchor, find, occurrence).map(|(range, _)| range)
}

/// [`locate_in`], and the items the anchor names (the whole file without
/// one), which the pre-flight counts: found once, for both.
pub(crate) fn locate_with_items(
    source: &str,
    all: &[Token],
    anchor: Option<&str>,
    find: &str,
    occurrence: Option<usize>,
) -> Result<(Range<usize>, Vec<Range<usize>>), Missing> {
    let wanted = tokens(find);
    if wanted.is_empty() {
        return Err(Missing::NothingToFind);
    }
    let wanted: Vec<&str> = wanted.iter().map(|token| token.text(find)).collect();

    let scopes = match anchor {
        Some(anchor) => items_of(source, all, anchor)?,
        None => std::iter::once(0..all.len()).collect(),
    };

    // The first token of each match. Scopes can overlap (an `impl` for a
    // type and a module of the same name), so a match is counted once.
    let mut matches: Vec<usize> = Vec::new();
    for scope in scopes.iter().cloned() {
        let Some(last_start) = scope.end.checked_sub(wanted.len()) else {
            continue;
        };
        for start in scope.start..=last_start {
            let here = &all[start..start + wanted.len()];
            if here
                .iter()
                .zip(&wanted)
                .all(|(token, wanted)| token.text(source) == *wanted)
                && !splits_an_operator(source, all, start, wanted.len())
            {
                matches.push(start);
            }
        }
    }
    matches.sort_unstable();
    matches.dedup();

    let range = |start: usize| all[start].start..all[start + wanted.len() - 1].end;
    match (matches.as_slice(), occurrence) {
        ([], _) => Err(Missing::NotFound),
        (found, Some(asked)) => match asked.checked_sub(1).and_then(|index| found.get(index)) {
            Some(start) => Ok((range(*start), scopes)),
            None => Err(Missing::NoSuchOccurrence {
                asked,
                found: found.len(),
            }),
        },
        ([only], None) => Ok((range(*only), scopes)),
        (several, None) => Err(Missing::Ambiguous(
            several
                .iter()
                .map(|start| line_of(source, all[*start].start))
                .collect(),
        )),
    }
}

/// The items an anchor names (`Round::spawn`), as token ranges: each
/// segment looked for inside the items the segments before it named.
/// Several when the name is not unique (two `impl` blocks with a `new`).
pub(crate) fn items_of(
    source: &str,
    all: &[Token],
    anchor: &str,
) -> Result<Vec<Range<usize>>, Missing> {
    let mut scopes: Vec<Range<usize>> = std::iter::once(0..all.len()).collect();
    let mut within = String::new();
    for segment in anchor.split("::").map(str::trim) {
        let mut inner = Vec::new();
        for scope in &scopes {
            // Inside an item, not the item itself: `impl fmt::Debug for Env`
            // answers to `fmt`, and `Env::fmt` means the function in it.
            let inside = if within.is_empty() {
                scope.clone()
            } else {
                scope.start + 1..scope.end
            };
            inner.extend(items_named(source, all, inside, segment));
        }
        if inner.is_empty() {
            return Err(Missing::NoSuchItem {
                segment: segment.to_owned(),
                within,
            });
        }
        // Scopes can overlap (an `impl` and a module of one name), and an
        // item found through both is one item.
        inner.sort_by_key(|range| (range.start, range.end));
        inner.dedup();
        scopes = inner;
        if !within.is_empty() {
            within.push_str("::");
        }
        within.push_str(segment);
    }
    Ok(scopes)
}

/// The words of a `cfg` that is about the system a build is for.
const SYSTEM_CFG: [&str; 5] = [
    "unix",
    "windows",
    "target_os",
    "target_family",
    "target_arch",
];

/// Whether the item that begins at token `start` has a `#[cfg(…)]` about
/// the system among the attributes before it: one of two items of a name,
/// one for each system, of which a build has only one. A `cfg` of a test
/// or a feature does not count: both of two such items can be built at
/// once.
pub(crate) fn under_cfg(source: &str, all: &[Token], start: usize) -> bool {
    let text = |index: usize| all[index].text(source);
    let mut at = start;
    // Back over what may come between the attributes and the keyword.
    while at > 0
        && matches!(
            text(at - 1),
            "pub" | "crate" | "super" | "in" | "(" | ")" | "unsafe" | "async" | "const" | "extern"
        )
    {
        at -= 1;
    }
    // Back over each attribute, `#[…]`.
    while at > 0 && text(at - 1) == "]" {
        let mut depth = 0usize;
        let mut open = None;
        for index in (0..at).rev() {
            match text(index) {
                "]" => depth += 1,
                "[" => {
                    depth -= 1;
                    if depth == 0 {
                        open = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(open) = open.filter(|open| *open > 0 && text(open - 1) == "#") else {
            return false;
        };
        if open + 1 < at
            && text(open + 1) == "cfg"
            && (open + 2..at).any(|index| SYSTEM_CFG.contains(&text(index)))
        {
            return true;
        }
        at = open - 1;
    }
    false
}

/// The tokens of `source`, a trailing comma included: what [`same_tokens`]
/// compares.
pub(crate) fn code_tokens(source: &str) -> Vec<Token> {
    lex(source, false)
}

/// [`tokens`], from the [`code_tokens`] of the same source: the commas that
/// only trail taken out. The same as lexing `source` again, for less.
pub(crate) fn without_layout_commas(source: &str, code: &[Token]) -> Vec<Token> {
    code.iter()
        .enumerate()
        .filter(|(index, token)| {
            token.text(source) != ","
                || !code
                    .get(index + 1)
                    .is_some_and(|next| matches!(next.text(source), ")" | "]" | "}"))
        })
        .map(|(_, token)| *token)
        .collect()
}

/// The text of each of [`code_tokens`].
pub(crate) fn code_texts(source: &str) -> Vec<&str> {
    lex(source, false)
        .into_iter()
        .map(|token| token.text(source))
        .collect()
}

/// The operators of two characters. Each character is a token of its own,
/// so without this a search for `= 0` would find the second half of
/// `x == 0`, and one for `> budget` the arrow of `Tier::Unit => budget`.
const OPERATORS: [&str; 20] = [
    "==", "!=", "<=", ">=", "=>", "->", "&&", "||", "::", "..", "<<", ">>", "+=", "-=", "*=", "/=",
    "%=", "^=", "|=", "&=",
];

/// Whether the tokens `start..start + length` begin with the second half of
/// an operator or end with the first half of one: the token before, or
/// after, touches them and makes an operator with the token at that end.
fn splits_an_operator(source: &str, all: &[Token], start: usize, length: usize) -> bool {
    let joined = |left: usize, right: usize| {
        all[left].end == all[right].start
            && OPERATORS.contains(&&source[all[left].start..all[right].end])
    };
    let end = start + length;
    (start > 0 && joined(start - 1, start)) || (end < all.len() && joined(end - 1, end))
}

/// Whether two sources are the same code: the same tokens, trailing commas
/// included. A search passes over a trailing comma, because the formatter
/// adds and removes one as it wraps a call. This does not, because one
/// trailing comma is code: `(x,)` is a tuple and `(x)` is `x`. A break that
/// only changes a comma is then taken for a change, and at worst is
/// reported as one no test caught.
pub(crate) fn same_tokens(a: &str, b: &str) -> bool {
    let (tokens_a, tokens_b) = (lex(a, false), lex(b, false));
    tokens_a.len() == tokens_b.len()
        && tokens_a
            .iter()
            .zip(&tokens_b)
            .all(|(x, y)| x.text(a) == y.text(b))
}

/// The line, counted from 1, that byte `at` of `source` is on.
pub(crate) fn line_of(source: &str, at: usize) -> usize {
    1 + source.as_bytes()[..at.min(source.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

/// The keywords that begin an item with a name after them.
const NAMED: [&str; 10] = [
    "fn",
    "mod",
    "struct",
    "enum",
    "union",
    "trait",
    "type",
    "const",
    "static",
    "macro_rules",
];

/// The items called `name` inside `scope`, at any depth, as token ranges
/// from the item's keyword to its end.
///
/// An `impl` block has no name of its own. It answers to every name in its
/// header: `impl Tool for Probe` is found as `Tool` and as `Probe`.
fn items_named(source: &str, all: &[Token], scope: Range<usize>, name: &str) -> Vec<Range<usize>> {
    let text = |index: usize| all[index].text(source);
    let mut found = Vec::new();
    for index in scope.clone() {
        let keyword = text(index);
        let is_item = if keyword == "impl" {
            // `impl` also begins a type (`x: impl Display`). An item comes
            // first in its file or block, or after another item, an
            // attribute or `unsafe`.
            let begins_an_item = index == scope.start
                || index == 0
                || matches!(text(index - 1), "}" | ";" | "]" | "{" | "unsafe");
            begins_an_item && {
                let header_end = (index..scope.end)
                    .find(|at| text(*at) == "{")
                    .unwrap_or(scope.end);
                (index + 1..header_end).any(|at| text(at) == name)
            }
        } else if NAMED.contains(&keyword) {
            // `macro_rules! name`
            let after = if keyword == "macro_rules" { 2 } else { 1 };
            index + after < scope.end && text(index + after) == name
        } else {
            false
        };
        if is_item {
            found.push(index..item_end(source, all, index, scope.end));
        }
    }
    found
}

/// The token after the last one of the item that begins at `start`: after
/// the brace that closes its body, or after the `;` that ends it.
fn item_end(source: &str, all: &[Token], start: usize, limit: usize) -> usize {
    let mut depth = 0usize;
    let mut in_body = false;
    for (index, token) in all.iter().enumerate().take(limit).skip(start) {
        match token.text(source) {
            "(" | "[" => depth += 1,
            ")" | "]" => depth = depth.saturating_sub(1),
            "{" => {
                depth += 1;
                in_body = true;
            }
            "}" => {
                depth = depth.saturating_sub(1);
                if in_body && depth == 0 {
                    return index + 1;
                }
            }
            ";" if depth == 0 => return index + 1,
            _ => {}
        }
    }
    limit
}

#[cfg(test)]
mod tests {
    use super::{Missing, line_of, locate, same_tokens, tokens};

    fn texts(source: &str) -> Vec<&str> {
        tokens(source)
            .iter()
            .map(|token| &source[token.start..token.end])
            .collect()
    }

    #[test]
    fn white_space_and_comments_are_not_tokens() {
        assert_eq!(
            texts("let x = a  +\n    b; // the sum\n/* and /* a nested */ one */ x"),
            ["let", "x", "=", "a", "+", "b", ";", "x"]
        );
    }

    #[test]
    fn a_doc_comment_is_a_comment() {
        assert_eq!(
            texts("/// Adds.\n//! Inner.\nfn add() {}"),
            ["fn", "add", "(", ")", "{", "}"]
        );
    }

    #[test]
    fn taking_layout_commas_out_of_the_code_tokens_is_lexing_without_them() {
        for source in [
            "f(a, b,)",
            "[1, 2,\n]; S { a, }",
            "f(a, // last\n)",
            "match x { 1 => a, 2 => b, }",
            "fn f() { g(\"a,)\", ',', /* , ) */ x,) }",
            include_str!("lex.rs"),
        ] {
            let code = super::code_tokens(source);
            assert_eq!(
                super::without_layout_commas(source, &code),
                tokens(source),
                "{source}"
            );
        }
    }

    #[test]
    fn a_trailing_comma_is_not_a_token_and_any_other_comma_is() {
        assert_eq!(texts("f(a, b,)"), ["f", "(", "a", ",", "b", ")"]);
        assert_eq!(
            texts("[1, 2,\n]; S { a, }"),
            ["[", "1", ",", "2", "]", ";", "S", "{", "a", "}"]
        );
        // Between a comma and the bracket there may be a comment.
        assert_eq!(texts("f(a, // last\n)"), ["f", "(", "a", ")"]);
    }

    #[test]
    fn strings_keep_what_is_in_them() {
        assert_eq!(
            texts(r#"say("a // b", 'x', b"}{", '\'', "q\"q")"#),
            [
                "say",
                "(",
                r#""a // b""#,
                ",",
                "'x'",
                ",",
                r#"b"}{""#,
                ",",
                r"'\''",
                ",",
                r#""q\"q""#,
                ")"
            ]
        );
    }

    #[test]
    fn a_raw_string_ends_at_its_own_closing_marks() {
        let source = r####"let a = r#"one " two"#; let b = br##"x"# y"##; r"plain""####;
        assert_eq!(
            texts(source),
            [
                "let",
                "a",
                "=",
                r###"r#"one " two"#"###,
                ";",
                "let",
                "b",
                "=",
                r###"br##"x"# y"##"###,
                ";",
                r#"r"plain""#
            ]
        );
    }

    #[test]
    fn a_lifetime_is_not_the_start_of_a_character() {
        assert_eq!(
            texts("fn f<'a>(x: &'a str) -> &'static str { 'outer: loop { break 'outer; } }"),
            [
                "fn", "f", "<", "'a", ">", "(", "x", ":", "&", "'a", "str", ")", "-", ">", "&",
                "'static", "str", "{", "'outer", ":", "loop", "{", "break", "'outer", ";", "}",
                "}"
            ]
        );
    }

    #[test]
    fn numbers_and_ranges_and_raw_names() {
        assert_eq!(
            texts("1.5 + 0x1f_u8 + 1..2 + 3.max(4) + r#type"),
            [
                "1.5", "+", "0x1f_u8", "+", "1", ".", ".", "2", "+", "3", ".", "max", "(", "4",
                ")", "+", "r#type"
            ]
        );
    }

    #[test]
    fn letters_outside_ascii_do_not_split_a_token() {
        assert_eq!(
            texts("let größe = 'ß'; \"→\""),
            ["let", "größe", "=", "'ß'", ";", "\"→\""]
        );
        // An unknown character is a token of its own, whole.
        assert_eq!(texts("a → b"), ["a", "→", "b"]);
    }

    #[test]
    fn something_left_open_ends_at_the_end_of_the_file() {
        assert_eq!(texts("a \"open"), ["a", "\"open"]);
        assert_eq!(texts("a /* open"), ["a"]);
        assert_eq!(texts("a r#\"open"), ["a", "r#\"open"]);
        assert_eq!(texts("a '\\"), ["a", "'\\"]);
    }

    const SOURCE: &str = r#"
impl Tier {
    /// The budget.
    fn budget(self) -> f64 {
        match self {
            Tier::Unit => 20.0,
            Tier::System if cfg!(windows) => 90.0,
        }
    }

    fn of(binary: &str, test: &str) -> Tier {
        if test.starts_with("sys_") {
            Tier::System
        } else {
            Tier::Unit
        }
    }
}

fn verdict(seconds: f64, budget: f64) -> bool {
    seconds > budget
}

mod tests {
    fn verdict() -> bool {
        1.0 > 2.0
    }
}
"#;

    fn found<'a>(
        source: &'a str,
        anchor: Option<&str>,
        find: &str,
        occurrence: Option<usize>,
    ) -> &'a str {
        let range = locate(source, anchor, find, occurrence)
            .unwrap_or_else(|missing| panic!("{find:?} in {anchor:?}: {missing:?}"));
        &source[range]
    }

    #[test]
    fn code_is_found_however_it_is_laid_out() {
        // As the formatter might have written the break's text when it was
        // one line, with a comment and a trailing comma of its own.
        let find =
            "if test . starts_with ( \"sys_\" , ) { Tier::System /* was */ } else { Tier::Unit }";

        assert_eq!(
            found(SOURCE, None, find, None),
            "if test.starts_with(\"sys_\") {\n            Tier::System\n        } else {\n            Tier::Unit\n        }"
        );
    }

    #[test]
    fn what_is_found_runs_from_the_first_token_to_the_last() {
        assert_eq!(
            found(SOURCE, None, "seconds > budget", None),
            "seconds > budget"
        );
    }

    #[test]
    fn code_that_is_there_twice_is_refused_with_the_lines() {
        assert_eq!(
            locate(SOURCE, None, "Tier::Unit", None),
            Err(Missing::Ambiguous(vec![6, 15]))
        );
    }

    #[test]
    fn an_item_narrows_the_search() {
        assert_eq!(found(SOURCE, Some("of"), "Tier::Unit", None), "Tier::Unit");
        let range = locate(SOURCE, Some("Tier::of"), "Tier::Unit", None).unwrap();
        assert_eq!(line_of(SOURCE, range.start), 15);
        let range = locate(SOURCE, Some("budget"), "Tier::Unit", None).unwrap();
        assert_eq!(line_of(SOURCE, range.start), 6);
    }

    #[test]
    fn an_item_is_found_inside_the_one_before_it_in_the_path() {
        // Two functions are called `verdict`. `bool {` is in both.
        assert_eq!(
            locate(SOURCE, Some("verdict"), "bool {", None),
            Err(Missing::Ambiguous(vec![20, 25]))
        );
        assert_eq!(
            found(SOURCE, Some("tests::verdict"), "1.0 > 2.0", None),
            "1.0 > 2.0"
        );
        let range = locate(SOURCE, Some("tests::verdict"), "bool {", None).unwrap();
        assert_eq!(line_of(SOURCE, range.start), 25);
    }

    #[test]
    fn an_occurrence_picks_one_of_several() {
        let second = locate(SOURCE, None, "Tier::Unit", Some(2)).unwrap();
        assert_eq!(line_of(SOURCE, second.start), 15);
        assert_eq!(
            locate(SOURCE, None, "Tier::Unit", Some(3)),
            Err(Missing::NoSuchOccurrence { asked: 3, found: 2 })
        );
        assert_eq!(
            locate(SOURCE, None, "Tier::Unit", Some(0)),
            Err(Missing::NoSuchOccurrence { asked: 0, found: 2 })
        );
    }

    #[test]
    fn code_that_is_not_there_is_not_found() {
        assert_eq!(
            locate(SOURCE, None, "seconds >= budget", None),
            Err(Missing::NotFound)
        );
        // It is in the file, but not in the item named.
        assert_eq!(
            locate(SOURCE, Some("budget"), "seconds > budget", None),
            Err(Missing::NotFound)
        );
        // Part of a token is not the token.
        assert_eq!(locate(SOURCE, None, "sys_", None), Err(Missing::NotFound));
        assert_eq!(locate(SOURCE, None, "second", None), Err(Missing::NotFound));
    }

    #[test]
    fn an_item_that_is_not_there_is_named() {
        assert_eq!(
            locate(SOURCE, Some("Tier::total"), "x", None),
            Err(Missing::NoSuchItem {
                segment: "total".to_owned(),
                within: "Tier".to_owned(),
            })
        );
        assert_eq!(
            locate(SOURCE, Some("Level"), "x", None),
            Err(Missing::NoSuchItem {
                segment: "Level".to_owned(),
                within: String::new(),
            })
        );
    }

    #[test]
    fn nothing_to_find_is_refused() {
        assert_eq!(locate(SOURCE, None, "", None), Err(Missing::NothingToFind));
        assert_eq!(
            locate(SOURCE, None, "  // only a comment\n", None),
            Err(Missing::NothingToFind)
        );
    }

    #[test]
    fn an_impl_answers_to_the_names_in_its_header() {
        let source = "
            impl Tool for Probe { fn run(&self) { go() } }
            impl Tool for Other { fn run(&self) { go() } }
            fn takes(x: impl Tool) { go() }
        ";

        assert_eq!(found(source, Some("Probe::run"), "go()", None), "go()");
        let probe = locate(source, Some("Probe::run"), "go()", None).unwrap();
        let other = locate(source, Some("Other::run"), "go()", None).unwrap();
        assert!(probe.start < other.start);
        // `impl Tool` in an argument is a type, not a block.
        assert_eq!(
            locate(source, Some("Tool"), "go()", None),
            Err(Missing::Ambiguous(vec![2, 3]))
        );
    }

    #[test]
    fn an_item_without_a_body_ends_at_its_semicolon() {
        let source = "const LIMIT: usize = [1, 2].len(); const OTHER: usize = 2;";

        assert_eq!(found(source, Some("LIMIT"), "2", None), "2");
        assert_eq!(found(source, Some("OTHER"), "2", None), "2");
        let source = "trait T { fn a(&self) -> u8; fn b(&self) -> u8 { 1 } }";
        assert_eq!(
            locate(source, Some("T::a"), "1", None),
            Err(Missing::NotFound)
        );
    }

    #[test]
    fn half_of_an_operator_is_not_found() {
        let source = "fn f(x: u8) -> u8 { if x == 0 { y = 0; } match x { 1 => budget, _ => x } }";

        // The assignment, and not the second half of the comparison.
        let range = locate(source, None, "= 0", None).unwrap();
        assert_eq!(&source[range.start - 2..range.end], "y = 0");
        // An arrow is not a comparison with what follows it.
        assert_eq!(
            locate(source, None, "> budget", None),
            Err(Missing::NotFound)
        );
        assert_eq!(locate(source, None, "> u8", None), Err(Missing::NotFound));
        // Nor is the first half of one a token to end at.
        assert_eq!(locate(source, None, "x =", None), Err(Missing::NotFound));
        // A whole operator is found, written with or without its space.
        assert_eq!(found(source, None, "x == 0", None), "x == 0");
        assert_eq!(found(source, None, "x = = 0", None), "x == 0");
        assert_eq!(found(source, None, "1 => budget", None), "1 => budget");
        // Brackets and other marks that only touch are not operators.
        assert_eq!(found("call(!(a))", None, "(a)", None), "(a)");
    }

    #[test]
    fn items_are_found_whatever_begins_or_surrounds_them() {
        let source = "
            unsafe impl Send for Probe { fn a(&self) { one() } }
            macro_rules! probe { () => { two() } }
            fn sized(x: [u8; 4], y: (u8, u8)) -> u8 { three() }
            fn after() { four() }
        ";

        assert_eq!(found(source, Some("Probe::a"), "one()", None), "one()");
        assert_eq!(found(source, Some("probe"), "two()", None), "two()");
        // The `;` inside the array's type does not end the function.
        assert_eq!(found(source, Some("sized"), "three()", None), "three()");
        assert_eq!(
            locate(source, Some("sized"), "four()", None),
            Err(Missing::NotFound)
        );
    }

    #[test]
    fn a_tuple_of_one_is_not_the_value_in_brackets() {
        assert!(!same_tokens("let pair = (x,);", "let pair = (x);"));
        assert!(!same_tokens("f(a, b)", "f(a, b,)"));
        // A search still passes over the comma the formatter adds to a
        // call it wraps.
        assert_eq!(
            found("f(\n    a,\n    b,\n)", None, "f(a, b)", None),
            "f(\n    a,\n    b,\n)"
        );
    }

    #[test]
    fn the_fields_of_a_tuple_in_a_tuple_are_two_names() {
        assert_eq!(
            texts("pair.0.1 + 0.1"),
            ["pair", ".", "0", ".", "1", "+", "0.1"]
        );
        assert_eq!(found("let x = pair.0.1;", None, "pair.0", None), "pair.0");
    }

    #[test]
    fn a_c_string_is_one_token() {
        assert_eq!(
            texts(r##"call(c"a } b", cr#"c " d"#)"##),
            ["call", "(", r#"c"a } b""#, ",", r##"cr#"c " d"#"##, ")"]
        );
    }

    #[test]
    fn code_inside_two_items_of_the_same_name_is_found_once() {
        // The function `a` is inside the module `a`: the code is in both.
        let source = "mod a { fn a() -> u8 { 7 } }";

        assert_eq!(found(source, Some("a"), "7", None), "7");
    }

    #[test]
    fn the_same_code_in_another_layout_is_the_same() {
        assert!(same_tokens("f(a, b)", "f(\n    a, // one\n    b\n)"));
        assert!(!same_tokens("f(a, b)", "f(a, c)"));
        assert!(!same_tokens("f(a, b)", "f(a, b"));
        assert!(!same_tokens("\"a b\"", "\"a  b\""));
    }
}
