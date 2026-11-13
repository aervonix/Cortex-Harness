//! What the configuration's untyped views may show of a value
//! (`docs/architecture/08-configuration.md`, "Credentials").
//!
//! `cortex config show`, `--layer`, [`Loaded::value`](crate::Loaded::value),
//! [`Loaded::effective`](crate::Loaded::effective), the `Debug` form of a
//! loaded configuration and what a load lists as held back for trust all
//! show values through [`redact`]. Three rules:
//!
//! 1. **A declared key that can hold a secret** (its kind is text or
//!    `{ env = "NAME" }`: today a provider's headers) shows its literal
//!    text as `[redacted]`. The typed settings keep it as a `Secret`.
//! 2. **In a reserved section**, whose keys a later step declares and which
//!    holds anything until then, a value is redacted whole when a key on
//!    its way names a secret by its look: `env` or `headers`, or a name
//!    with `token`, `secret`, `password`, `auth` or `credential` in it, or
//!    a part that is `key` or ends in `apikey` (`api_key`, `X-Api-Key`).
//!    It is a guess, made on the safe side, and the step that declares the
//!    section replaces it with the first rule.
//! 3. **In any text, a web address's user part is redacted:**
//!    `http://user:password@proxy` shows as `http://[redacted]@proxy`.

use toml::Value;

use super::{Kind, Lookup, Node, lookup};
use crate::key::KeyPath;

/// What a literal that may be a secret is shown as.
fn redacted() -> Value {
    Value::String(cortex_base::secret::REDACTED.to_owned())
}

/// The value at `key` as the configuration's untyped views may show it.
/// A table is gone into, so a table that holds a secret is redacted where
/// it holds it.
#[must_use]
pub(crate) fn redact(key: &KeyPath, value: &Value) -> Value {
    let shown = match lookup(key) {
        Lookup::Setting { child, .. } => match &child.node {
            Node::Leaf(kind) => redact_kind(kind, value),
            _ => value.clone(),
        },
        // One entry of such a setting: a single header.
        Lookup::InsideSetting { child } => match &child.node {
            Node::Leaf(kind) if may_hold_secret(kind) => redact_kind(&Kind::EnvText, value),
            _ => value.clone(),
        },
        Lookup::Table { .. } => match value {
            Value::Table(table) => Value::Table(
                table
                    .iter()
                    .map(|(name, entry)| (name.clone(), redact(&key.child(name), entry)))
                    .collect(),
            ),
            _ => value.clone(),
        },
        Lookup::Reserved { .. } => redact_reserved(key, value),
        Lookup::Unknown { .. } => value.clone(),
    };
    hide_url_users(&shown)
}

/// A section whose keys are not declared yet, as its `Debug` form shows it:
/// rules 2 and 3.
pub(crate) fn redact_undeclared(table: &toml::Table) -> Value {
    hide_url_users(&redact_reserved(
        &KeyPath::root(),
        &Value::Table(table.clone()),
    ))
}

/// A field of a settings struct as the struct's `Debug` form shows it: the
/// field's own `Debug`, with the user part of any web address in it
/// redacted (rule 3). Rule 1 needs nothing here: a literal that may be a
/// secret is a `Secret`, whose `Debug` is `[redacted]`.
pub(crate) struct Shown<'a, T: ?Sized>(pub(crate) &'a T);

impl<T: std::fmt::Debug + ?Sized> std::fmt::Debug for Shown<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = if f.alternate() {
            format!("{:#?}", self.0)
        } else {
            format!("{:?}", self.0)
        };
        f.write_str(&without_url_users(&text).unwrap_or(text))
    }
}

/// Whether a setting of this kind can hold a secret as literal text.
pub(crate) fn may_hold_secret(kind: &Kind) -> bool {
    match kind {
        Kind::EnvText => true,
        Kind::List(inner) | Kind::Map(inner) => may_hold_secret(inner),
        _ => false,
    }
}

/// A value of `kind` with every literal that may be a secret replaced by
/// `[redacted]`. `{ env = "NAME" }` is kept: a name is not a secret.
pub(crate) fn redact_kind(kind: &Kind, value: &Value) -> Value {
    match (kind, value) {
        (Kind::EnvText, Value::String(_)) => redacted(),
        (Kind::Map(inner), Value::Table(table)) => Value::Table(
            table
                .iter()
                .map(|(name, entry)| (name.clone(), redact_kind(inner, entry)))
                .collect(),
        ),
        (Kind::List(inner), Value::Array(items)) => {
            Value::Array(items.iter().map(|item| redact_kind(inner, item)).collect())
        }
        _ => value.clone(),
    }
}

/// A value in a reserved section: redacted whole under a key that names a
/// secret, and otherwise gone into.
fn redact_reserved(key: &KeyPath, value: &Value) -> Value {
    if key.parts().iter().any(|part| names_a_secret(part)) {
        return redact_all(value);
    }
    match value {
        Value::Table(table) => Value::Table(
            table
                .iter()
                .map(|(name, entry)| (name.clone(), redact_reserved(&key.child(name), entry)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// Every text in a value redacted, its shape kept.
fn redact_all(value: &Value) -> Value {
    match value {
        Value::String(_) => redacted(),
        Value::Table(table) => Value::Table(
            table
                .iter()
                .map(|(name, entry)| (name.clone(), redact_all(entry)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_all).collect()),
        _ => value.clone(),
    }
}

/// Whether a key's name looks like it names a secret, or a table of them.
fn names_a_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if matches!(name.as_str(), "env" | "headers") {
        return true;
    }
    if [
        "token",
        "secret",
        "password",
        "passwd",
        "auth",
        "credential",
    ]
    .iter()
    .any(|word| name.contains(word))
    {
        return true;
    }
    // `key` as a word of its own: `api_key`, `X-Api-Key`, `apiKey`, and
    // not `keymap` or `hotkey`, which name key bindings.
    name.split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| part == "key" || part.ends_with("apikey"))
}

/// Every text in a value with a web address's user part redacted.
fn hide_url_users(value: &Value) -> Value {
    match value {
        Value::String(text) => match without_url_users(text) {
            Some(hidden) => Value::String(hidden),
            None => value.clone(),
        },
        Value::Table(table) => Value::Table(
            table
                .iter()
                .map(|(name, entry)| (name.clone(), hide_url_users(entry)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(hide_url_users).collect()),
        _ => value.clone(),
    }
}

/// `text` with the user part of each address in it (`scheme://user:pass@`)
/// replaced by `[redacted]`. `None` when there is none.
fn without_url_users(text: &str) -> Option<String> {
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(start) = rest.find("://") {
        let (before, after) = rest.split_at(start + 3);
        shown.push_str(before);
        // The address's authority ends where its path, query or fragment
        // begins, or at a space or a quotation mark, neither of which it can
        // hold: a `Debug` form puts the address in quotes.
        let end = after
            .find(|character: char| {
                matches!(character, '/' | '?' | '#' | '"' | '\\') || character.is_whitespace()
            })
            .unwrap_or(after.len());
        let authority = &after[..end];
        match authority.rfind('@') {
            Some(at) => {
                shown.push_str(cortex_base::secret::REDACTED);
                shown.push_str(&authority[at..]);
                changed = true;
            }
            None => shown.push_str(authority),
        }
        rest = &after[end..];
    }
    shown.push_str(rest);
    changed.then_some(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toml(text: &str) -> Value {
        let table: toml::Table = format!("value = {text}").parse().unwrap();
        table["value"].clone()
    }

    fn key(text: &str) -> KeyPath {
        KeyPath::parse(text).unwrap()
    }

    #[test]
    fn a_declared_literal_is_redacted_and_a_variable_name_kept() {
        let kind = Kind::Map(Box::new(Kind::EnvText));
        assert!(may_hold_secret(&kind) && !may_hold_secret(&Kind::Text));
        assert_eq!(
            redact_kind(
                &kind,
                &toml("{ api-key = \"k\", X-Two = { env = \"TWO\" } }")
            ),
            toml("{ api-key = \"[redacted]\", X-Two = { env = \"TWO\" } }")
        );
        assert_eq!(
            redact_kind(&Kind::List(Box::new(Kind::EnvText)), &toml("[\"k\"]")),
            toml("[\"[redacted]\"]")
        );
        assert_eq!(redact_kind(&Kind::Text, &toml("\"k\"")), toml("\"k\""));
        assert_eq!(
            redact(&key("providers.x.headers"), &toml("{ A = \"k\" }")),
            toml("{ A = \"[redacted]\" }")
        );
        assert_eq!(
            redact(&key("providers.x.headers.A"), &toml("\"k\"")),
            toml("\"[redacted]\"")
        );
        // A table that holds the setting is redacted where it holds it.
        assert_eq!(
            redact(
                &key("providers"),
                &toml("{ x = { base_url = \"u\", headers = { A = \"k\" } } }")
            ),
            toml("{ x = { base_url = \"u\", headers = { A = \"[redacted]\" } } }")
        );
    }

    #[test]
    fn in_a_reserved_section_a_secret_is_known_by_its_name() {
        for name in [
            "env",
            "headers",
            "Headers",
            "token",
            "access_token",
            "authToken",
            "client_secret",
            "password",
            "passwd",
            "Authorization",
            "oauth",
            "credentials",
            "api_key",
            "X-Api-Key",
            "apiKey",
            "key",
        ] {
            assert!(names_a_secret(name), "{name}");
        }
        for name in [
            "command", "args", "url", "theme", "keymap", "hotkey", "keys", "monkey", "servers",
        ] {
            assert!(!names_a_secret(name), "{name}");
        }

        let servers = toml(
            "{ x = { command = \"srv\", env = { TOKEN = \"k\", MODE = \"m\" }, api_key = \"k\" } }",
        );
        assert_eq!(
            redact(&key("mcp.servers"), &servers),
            toml(
                "{ x = { command = \"srv\", env = { TOKEN = \"[redacted]\", MODE = \
                 \"[redacted]\" }, api_key = \"[redacted]\" } }"
            )
        );
        // The same, flattened to a key a line as `config show` prints it.
        assert_eq!(
            redact(&key("mcp.servers.x.env.MODE"), &toml("\"m\"")),
            toml("\"[redacted]\"")
        );
        assert_eq!(
            redact(&key("mcp.servers.x.command"), &toml("\"srv\"")),
            toml("\"srv\"")
        );
        // Key bindings are not secrets.
        assert_eq!(
            redact(
                &KeyPath::new(["keys", "normal", "ctrl+p"]),
                &toml("\"palette\"")
            ),
            toml("\"palette\"")
        );
    }

    #[test]
    fn the_user_part_of_a_web_address_is_redacted_anywhere() {
        for (text, shown) in [
            (
                "http://me:pw@proxy:8080",
                Some("http://[redacted]@proxy:8080"),
            ),
            (
                "https://tok@host/p?q=1#f",
                Some("https://[redacted]@host/p?q=1#f"),
            ),
            (
                "a http://u:p@one/x then socks5://v@two",
                Some("a http://[redacted]@one/x then socks5://[redacted]@two"),
            ),
            ("https://host/path@not-user", None),
            ("http://host", None),
            ("git@github.com:org/repo", None),
            ("no address", None),
            ("://@", Some("://[redacted]@")),
        ] {
            assert_eq!(without_url_users(text).as_deref(), shown, "{text}");
        }
        // In a declared key and in a reserved one, and inside a list.
        assert_eq!(
            redact(&key("providers.x.base_url"), &toml("\"https://u:p@gw/v1\"")),
            toml("\"https://[redacted]@gw/v1\"")
        );
        assert_eq!(
            redact(&key("http.proxy"), &toml("\"http://u:p@proxy\"")),
            toml("\"http://[redacted]@proxy\"")
        );
        assert_eq!(
            redact(&key("sandbox.write"), &toml("[\"a\", \"ftp://u@h/x\"]")),
            toml("[\"a\", \"ftp://[redacted]@h/x\"]")
        );
    }
}
