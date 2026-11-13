//! [`KeyPath`]: the name of a setting, as a list of table names.
//!
//! `model.main` is the key `main` in the table `model`. A path is kept as
//! its parts, not as dotted text, because a name may itself contain a dot:
//! `[providers."my.server"]` is two parts, not three.

use std::fmt;

/// The name of a setting or of a table of settings.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyPath(Vec<String>);

/// Why a key path could not be read from text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is not a key: {reason}")]
pub struct KeyPathError {
    /// What was given.
    pub text: String,
    /// What is wrong with it.
    pub reason: &'static str,
}

impl KeyPath {
    /// The path with no parts: the file itself.
    #[must_use]
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// A path from its parts.
    #[must_use]
    pub fn new<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(parts.into_iter().map(Into::into).collect())
    }

    /// Reads a path as it is written in a TOML file: parts separated by
    /// dots, and a part that is not plain letters, digits, `_` and `-`
    /// inside quotes.
    ///
    /// # Errors
    ///
    /// When the text is empty, has an empty part, or a quote is not closed.
    pub fn parse(text: &str) -> Result<Self, KeyPathError> {
        let fail = |reason| KeyPathError {
            text: text.to_owned(),
            reason,
        };
        let mut parts = Vec::new();
        let mut rest = text.trim();
        if rest.is_empty() {
            return Err(fail("it is empty"));
        }
        loop {
            let (part, after) = match rest.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let body = &rest[1..];
                    let mut part = String::new();
                    let mut end = None;
                    let mut characters = body.char_indices();
                    while let Some((index, character)) = characters.next() {
                        if character == quote {
                            end = Some(index);
                            break;
                        }
                        // Only the double-quoted form has escapes, and only
                        // the two a key can need.
                        if character == '\\' && quote == '"' {
                            match characters.next() {
                                Some((_, escaped @ ('"' | '\\'))) => part.push(escaped),
                                _ => {
                                    return Err(fail(
                                        "only `\\\"` and `\\\\` may follow a backslash",
                                    ));
                                }
                            }
                        } else {
                            part.push(character);
                        }
                    }
                    let end = end.ok_or_else(|| fail("a quote is not closed"))?;
                    (part, &body[end + 1..])
                }
                _ => {
                    let length = rest
                        .find(|character: char| !is_bare(character))
                        .unwrap_or(rest.len());
                    if length == 0 {
                        return Err(fail("a part is empty or has a character that needs quotes"));
                    }
                    (rest[..length].to_owned(), &rest[length..])
                }
            };
            parts.push(part);
            let after = after.trim_start();
            if after.is_empty() {
                return Ok(Self(parts));
            }
            rest = after
                .strip_prefix('.')
                .ok_or_else(|| fail("parts are separated by dots"))?
                .trim_start();
            if rest.is_empty() {
                return Err(fail("it ends with a dot"));
            }
        }
    }

    /// The parts, outermost first.
    #[must_use]
    pub fn parts(&self) -> &[String] {
        &self.0
    }

    /// Whether this is the root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// How many parts the path has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the path has no parts. The same as [`KeyPath::is_root`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The first part: the section.
    #[must_use]
    pub fn first(&self) -> Option<&str> {
        self.0.first().map(String::as_str)
    }

    /// The last part: the key's own name.
    #[must_use]
    pub fn last(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }

    /// The path of `name` inside this one.
    #[must_use]
    pub fn child(&self, name: &str) -> Self {
        let mut parts = Vec::with_capacity(self.0.len() + 1);
        parts.extend(self.0.iter().cloned());
        parts.push(name.to_owned());
        Self(parts)
    }

    /// The path this one is inside, or `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let (_, rest) = self.0.split_last()?;
        Some(Self(rest.to_vec()))
    }

    /// Whether this path is `other` or is inside it. A lock on a table
    /// covers every key in it.
    #[must_use]
    pub fn is_at_or_under(&self, other: &Self) -> bool {
        self.0.len() >= other.0.len() && self.0[..other.0.len()] == other.0[..]
    }

    /// The path of `other` inside this one: this path's parts, then
    /// `other`'s.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self(self.0.iter().chain(&other.0).cloned().collect())
    }

    /// This path without its first `count` parts.
    #[must_use]
    pub fn without_first(&self, count: usize) -> Self {
        Self(self.0.iter().skip(count).cloned().collect())
    }
}

/// Whether a character may be in a part written without quotes.
fn is_bare(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '-'
}

impl fmt::Display for KeyPath {
    /// As it would be written in a TOML file. The root prints as nothing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, part) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            if !part.is_empty() && part.chars().all(is_bare) {
                f.write_str(part)?;
            } else {
                f.write_str("\"")?;
                for character in part.chars() {
                    if character == '"' || character == '\\' {
                        f.write_str("\\")?;
                    }
                    write!(f, "{character}")?;
                }
                f.write_str("\"")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for KeyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyPath({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_reads_and_prints_the_same_way() {
        for (text, parts) in [
            ("model", vec!["model"]),
            ("model.main", vec!["model", "main"]),
            ("mcp.servers.git-hub_2", vec!["mcp", "servers", "git-hub_2"]),
            (
                r#"providers."my.server".base_url"#,
                vec!["providers", "my.server", "base_url"],
            ),
            (
                r#"providers."a \"b\" \\ c""#,
                vec!["providers", r#"a "b" \ c"#],
            ),
            (r#"keys."ctrl+x""#, vec!["keys", "ctrl+x"]),
        ] {
            let path = KeyPath::parse(text).unwrap();
            assert_eq!(path.parts(), parts, "{text}");
            assert_eq!(path.to_string(), text);
            assert_eq!(KeyPath::parse(&path.to_string()).unwrap(), path);
        }
        // Spaces around the dots and a literal-quoted part are accepted, and
        // print in the one canonical form.
        let path = KeyPath::parse(" providers . 'my.server' ").unwrap();
        assert_eq!(path.to_string(), r#"providers."my.server""#);
    }

    #[test]
    fn what_is_not_a_path_is_refused_with_a_reason() {
        for text in [
            "",
            "   ",
            ".model",
            "model.",
            "model..main",
            "model main",
            "model=1",
            "model.ma in",
            r#"providers."open"#,
            r#"providers."bad \n escape""#,
            "model.main # note",
        ] {
            let error = KeyPath::parse(text).unwrap_err();
            assert_eq!(error.text, text);
            assert!(error.to_string().contains("is not a key"), "{error}");
        }
    }

    #[test]
    fn a_path_knows_what_it_is_inside() {
        let key = KeyPath::parse("permissions.rules").unwrap();
        let table = KeyPath::parse("permissions").unwrap();
        assert!(key.is_at_or_under(&table));
        assert!(key.is_at_or_under(&key));
        assert!(key.is_at_or_under(&KeyPath::root()));
        assert!(!table.is_at_or_under(&key));
        // A name that merely begins the same way is another key.
        assert!(
            !KeyPath::parse("permissions_extra")
                .unwrap()
                .is_at_or_under(&table)
        );

        assert_eq!(key.parent().unwrap(), table);
        assert_eq!(table.parent().unwrap(), KeyPath::root());
        assert!(KeyPath::root().parent().is_none());
        assert_eq!(table.child("rules"), key);
        assert_eq!(key.first(), Some("permissions"));
        assert_eq!(key.last(), Some("rules"));
        assert_eq!(key.without_first(1), KeyPath::new(["rules"]));
        assert_eq!(table.join(&KeyPath::new(["rules"])), key);
        assert_eq!(KeyPath::root().join(&key), key);
        assert_eq!((key.len(), KeyPath::root().is_root()), (2, true));
        assert_eq!(format!("{key:?}"), "KeyPath(permissions.rules)");
    }

    #[test]
    fn an_empty_name_prints_in_quotes() {
        assert_eq!(KeyPath::new([""]).to_string(), r#""""#);
    }
}
