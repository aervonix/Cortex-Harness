//! The kinds of value a setting can hold, and how each is read from TOML.

use std::collections::BTreeMap;
use std::time::Duration;

use cortex_base::Secret;
use cortex_protocol::lease::{NetworkAccess, Rule};
use cortex_protocol::{Level, Speed};
use toml::Value;

use super::{Kind, Node, Setting, choice};
use crate::duration::{format_duration, parse_duration};
use crate::env::Env;

/// "expected one of `a`, `b` or `c`".
pub(crate) fn one_of(words: &[&str]) -> String {
    let quoted: Vec<String> = words.iter().map(|word| format!("`{word}`")).collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => {
            format!("expected one of {} or {last}", rest.join(", "))
        }
        Some((only, _)) => format!("expected {only}"),
        None => "no value is accepted".to_owned(),
    }
}

/// A value as a size that can be compared, for the settings a project may
/// only tighten: a number as itself, a length of time in milliseconds.
pub(crate) fn magnitude(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(number) => Some(*number as f64),
        Value::Float(number) => Some(*number),
        Value::String(text) => parse_duration(text)
            .ok()
            .map(|duration| duration.as_secs_f64() * 1000.0),
        _ => None,
    }
}

impl Setting for bool {
    fn node() -> Node {
        Node::Leaf(Kind::Bool)
    }

    fn read(value: &Value) -> Result<Self, String> {
        value
            .as_bool()
            .ok_or_else(|| "expected `true` or `false`".to_owned())
    }

    fn write(&self) -> Option<Value> {
        Some(Value::Boolean(*self))
    }
}

impl Setting for u32 {
    fn node() -> Node {
        Node::Leaf(Kind::Integer)
    }

    fn read(value: &Value) -> Result<Self, String> {
        value
            .as_integer()
            .and_then(|number| Self::try_from(number).ok())
            .ok_or_else(|| "expected a whole number that is not negative".to_owned())
    }

    fn write(&self) -> Option<Value> {
        Some(Value::Integer(i64::from(*self)))
    }
}

impl Setting for u64 {
    fn node() -> Node {
        Node::Leaf(Kind::Integer)
    }

    fn read(value: &Value) -> Result<Self, String> {
        value
            .as_integer()
            .and_then(|number| Self::try_from(number).ok())
            .ok_or_else(|| "expected a whole number that is not negative".to_owned())
    }

    fn write(&self) -> Option<Value> {
        // TOML's whole numbers stop at the largest `i64`.
        Some(Value::Integer(i64::try_from(*self).unwrap_or(i64::MAX)))
    }
}

impl Setting for f64 {
    fn node() -> Node {
        Node::Leaf(Kind::Number)
    }

    fn read(value: &Value) -> Result<Self, String> {
        let number = match value {
            Value::Integer(number) => Some(*number as Self),
            Value::Float(number) => Some(*number),
            _ => None,
        };
        number
            .filter(|number| number.is_finite())
            .ok_or_else(|| "expected a number".to_owned())
    }

    fn write(&self) -> Option<Value> {
        Some(Value::Float(*self))
    }
}

impl Setting for String {
    fn node() -> Node {
        Node::Leaf(Kind::Text)
    }

    fn read(value: &Value) -> Result<Self, String> {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "expected text in quotes".to_owned())
    }

    fn write(&self) -> Option<Value> {
        Some(Value::String(self.clone()))
    }
}

impl Setting for Duration {
    fn node() -> Node {
        Node::Leaf(Kind::Duration)
    }

    fn read(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(text) => parse_duration(text),
            _ => parse_duration(""),
        }
    }

    fn write(&self) -> Option<Value> {
        Some(Value::String(format_duration(*self)))
    }
}

impl<T: Setting> Setting for Vec<T> {
    fn node() -> Node {
        match T::node() {
            Node::Leaf(kind) => Node::Leaf(Kind::List(Box::new(kind))),
            // A list of tables is declared as its own kind, like `Rule`.
            other => other,
        }
    }

    fn read(value: &Value) -> Result<Self, String> {
        let Value::Array(items) = value else {
            return Err("expected a list in square brackets".to_owned());
        };
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                T::read(item).map_err(|error| format!("entry {}: {error}", index + 1))
            })
            .collect()
    }

    fn write(&self) -> Option<Value> {
        Some(Value::Array(
            self.iter().filter_map(Setting::write).collect(),
        ))
    }
}

/// A speed tier is a name a model offers, so any name is accepted here and
/// checked against the model's offers when it is used (decision 0027).
impl Setting for Speed {
    fn node() -> Node {
        Node::Leaf(Kind::Text)
    }

    fn read(value: &Value) -> Result<Self, String> {
        match value.as_str() {
            Some(name) if !name.trim().is_empty() => Ok(Self::from_wire(name)),
            _ => Err(
                "expected the name of a speed tier, such as \"standard\" or \"fast\"".to_owned(),
            ),
        }
    }

    fn write(&self) -> Option<Value> {
        Some(Value::String(self.as_str().to_owned()))
    }
}

/// A permission mode is enforced, so only the levels this build knows are
/// accepted (decision 0028).
impl Setting for Level {
    fn node() -> Node {
        Node::Leaf(Kind::Choice(Self::KNOWN))
    }

    fn read(value: &Value) -> Result<Self, String> {
        match value.as_str() {
            Some(name) if Self::KNOWN.contains(&name) => Ok(Self::from_wire(name)),
            _ => Err(one_of(Self::KNOWN)),
        }
    }

    fn write(&self) -> Option<Value> {
        Some(Value::String(self.as_str().to_owned()))
    }
}

impl Setting for NetworkAccess {
    fn node() -> Node {
        Node::Leaf(Kind::Choice(Self::KNOWN))
    }

    fn read(value: &Value) -> Result<Self, String> {
        value
            .as_str()
            .and_then(Self::from_wire)
            .ok_or_else(|| one_of(Self::KNOWN))
    }

    fn write(&self) -> Option<Value> {
        Some(Value::String(self.as_str().to_owned()))
    }
}

/// A permission rule is read by the protocol's own strict reader, which
/// refuses a field or a value it does not know (decision 0028). This crate
/// adds no lenient path beside it.
impl Setting for Rule {
    fn node() -> Node {
        Node::Leaf(Kind::Rule)
    }

    fn read(value: &Value) -> Result<Self, String> {
        if !value.is_table() {
            return Err("expected a rule: a table with `effect`, `match` and `action`".to_owned());
        }
        value
            .clone()
            .try_into::<Self>()
            .map_err(|error| format!("not a rule this version understands: {}", error.message()))
    }

    fn write(&self) -> Option<Value> {
        let mut table = toml::Table::new();
        table.insert(
            "effect".to_owned(),
            Value::String(self.effect.as_str().to_owned()),
        );
        table.insert("match".to_owned(), Value::String(self.pattern.clone()));
        table.insert(
            "action".to_owned(),
            Value::String(self.action.as_str().to_owned()),
        );
        Some(Value::Table(table))
    }
}

/// Text, or the name of an environment variable to read when the value is
/// used: `"abc"` or `{ env = "NAME" }`.
///
/// There is no other indirection: no interpolation inside strings, and no
/// "run this command to get a value".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvText {
    /// The text itself.
    Literal(String),
    /// The name of an environment variable.
    Env(String),
}

impl EnvText {
    /// The value, as a [`Secret`]: a header value that came from the
    /// environment is one, and a literal is treated the same way so that
    /// neither is logged. `None` when the variable is not set.
    #[must_use]
    pub fn resolve(&self, env: &Env) -> Option<Secret> {
        match self {
            Self::Literal(text) => Some(Secret::new(text.clone())),
            Self::Env(name) => env.get(name).map(Secret::new),
        }
    }
}

impl Setting for EnvText {
    fn node() -> Node {
        Node::Leaf(Kind::EnvText)
    }

    fn read(value: &Value) -> Result<Self, String> {
        const EXPECTED: &str =
            "expected text, or `{ env = \"NAME\" }` to read an environment variable";
        match value {
            Value::String(text) => Ok(Self::Literal(text.clone())),
            Value::Table(table) if table.len() == 1 => match table.get("env") {
                Some(Value::String(name)) if !name.is_empty() => Ok(Self::Env(name.clone())),
                _ => Err(EXPECTED.to_owned()),
            },
            _ => Err(EXPECTED.to_owned()),
        }
    }

    fn write(&self) -> Option<Value> {
        Some(match self {
            Self::Literal(text) => Value::String(text.clone()),
            Self::Env(name) => {
                let mut table = toml::Table::new();
                table.insert("env".to_owned(), Value::String(name.clone()));
                Value::Table(table)
            }
        })
    }
}

/// Extra request headers for a provider: a table from header name to value.
/// It is one setting: a layer that sets it sets all of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(pub BTreeMap<String, EnvText>);

impl Setting for Headers {
    fn node() -> Node {
        Node::Leaf(Kind::Map(Box::new(Kind::EnvText)))
    }

    fn read(value: &Value) -> Result<Self, String> {
        let Value::Table(table) = value else {
            return Err("expected a table from header names to values".to_owned());
        };
        table
            .iter()
            .map(|(name, value)| {
                EnvText::read(value)
                    .map(|value| (name.clone(), value))
                    .map_err(|error| format!("{name}: {error}"))
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    fn write(&self) -> Option<Value> {
        if self.0.is_empty() {
            return None;
        }
        Some(Value::Table(
            self.0
                .iter()
                .filter_map(|(name, value)| Some((name.clone(), value.write()?)))
                .collect(),
        ))
    }
}

/// A section whose keys are not declared yet: the build step that owns the
/// section declares them. Until then whatever a file holds there is kept as
/// it was written and not checked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reserved(pub toml::Table);

impl Setting for Reserved {
    fn node() -> Node {
        Node::Open
    }

    fn read(value: &Value) -> Result<Self, String> {
        match value {
            Value::Table(table) => Ok(Self(table.clone())),
            _ => Err("expected a table".to_owned()),
        }
    }

    fn write(&self) -> Option<Value> {
        (!self.0.is_empty()).then(|| Value::Table(self.0.clone()))
    }
}

/// The profiles a file defines: named tables of settings. They are applied
/// while layers are merged and are not part of the result, so the settings
/// read from a merged configuration hold none.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profiles;

impl Setting for Profiles {
    fn node() -> Node {
        Node::Profiles
    }

    fn read(_: &Value) -> Result<Self, String> {
        Ok(Self)
    }

    fn write(&self) -> Option<Value> {
        None
    }
}

choice! {
    /// How much of a model's reasoning is shown.
    pub enum ReasoningVisibility {
        /// A summary, where the provider can give one.
        Summary => "summary",
        /// Nothing.
        None => "none",
    }
}

choice! {
    /// Where stored credentials are kept.
    pub enum StoreChoice {
        /// The default for the OS: Windows Credential Manager on Windows, an
        /// owner-only file on Linux.
        Auto => "auto",
        /// An owner-only file, `credentials.toml`, in the configuration
        /// directory.
        File => "file",
        /// The OS keychain: Windows Credential Manager, or on Linux the
        /// Secret Service in a build that has it.
        Keychain => "keychain",
    }
}

choice! {
    /// When the session log is flushed to the disk.
    pub enum SessionDurability {
        /// After every item: a crash loses at most the item being written.
        Item => "item",
        /// At the end of every turn: a crash of the machine can lose the
        /// turn in progress.
        Turn => "turn",
        /// Never: the operating system writes the log when it chooses. A
        /// crash of the machine can lose anything not yet written.
        Never => "never",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Platform;

    fn toml(text: &str) -> Value {
        let table: toml::Table = format!("value = {text}").parse().unwrap();
        table["value"].clone()
    }

    #[test]
    fn each_kind_reads_its_own_values_and_says_what_it_expects() {
        assert!(bool::read(&toml("true")).unwrap());
        assert!(
            bool::read(&toml("1"))
                .unwrap_err()
                .contains("`true` or `false`")
        );

        assert_eq!(u32::read(&toml("5")).unwrap(), 5);
        for wrong in ["-1", "5.0", "\"5\"", "4294967296"] {
            assert!(u32::read(&toml(wrong)).is_err(), "{wrong}");
        }
        assert_eq!(u64::read(&toml("4294967296")).unwrap(), 4_294_967_296);
        assert!(u64::read(&toml("-1")).is_err());

        assert_eq!(f64::read(&toml("2")).unwrap(), 2.0);
        assert_eq!(f64::read(&toml("2.5")).unwrap(), 2.5);
        for wrong in ["inf", "nan", "\"2\"", "true"] {
            assert!(f64::read(&toml(wrong)).is_err(), "{wrong}");
        }

        assert_eq!(String::read(&toml("\"x\"")).unwrap(), "x");
        assert!(String::read(&toml("5")).is_err());

        assert_eq!(
            Duration::read(&toml("\"1m30s\"")).unwrap(),
            Duration::from_secs(90)
        );
        for wrong in ["90", "\"90\"", "true"] {
            assert!(
                Duration::read(&toml(wrong))
                    .unwrap_err()
                    .contains("length of time"),
                "{wrong}"
            );
        }

        assert_eq!(
            Vec::<String>::read(&toml("[\"a\", \"b\"]")).unwrap(),
            ["a", "b"]
        );
        let error = Vec::<String>::read(&toml("[\"a\", 2]")).unwrap_err();
        assert!(error.starts_with("entry 2:"), "{error}");
        assert!(Vec::<String>::read(&toml("\"a\"")).is_err());
    }

    #[test]
    fn a_value_writes_back_as_it_was_read() {
        for text in ["true", "5", "2.5", "\"x\"", "[\"a\", \"b\"]"] {
            let value = toml(text);
            let written = match &value {
                Value::Boolean(_) => bool::read(&value).unwrap().write(),
                Value::Integer(_) => u64::read(&value).unwrap().write(),
                Value::Float(_) => f64::read(&value).unwrap().write(),
                Value::String(_) => String::read(&value).unwrap().write(),
                _ => Vec::<String>::read(&value).unwrap().write(),
            };
            assert_eq!(written, Some(value), "{text}");
        }
        assert_eq!(
            Duration::from_secs(90).write(),
            Some(Value::String("1m30s".to_owned()))
        );
        assert_eq!(Option::<u32>::None.write(), None);
        assert_eq!(Some(3_u32).write(), Some(Value::Integer(3)));
        assert_eq!(u64::MAX.write(), Some(Value::Integer(i64::MAX)));
    }

    #[test]
    fn a_speed_is_any_name_and_a_mode_is_a_known_level() {
        assert_eq!(Speed::read(&toml("\"fast\"")).unwrap(), Speed::Fast);
        assert_eq!(
            Speed::read(&toml("\"flex\"")).unwrap(),
            Speed::from_wire("flex")
        );
        assert!(Speed::read(&toml("\"\"")).is_err());
        assert!(Speed::read(&toml("true")).is_err());

        assert_eq!(
            Level::read(&toml("\"read-only\"")).unwrap(),
            Level::ReadOnly
        );
        let error = Level::read(&toml("\"yolo\"")).unwrap_err();
        assert!(
            error.contains("`supervised`") && error.contains("`unrestricted`"),
            "{error}"
        );

        assert_eq!(
            NetworkAccess::read(&toml("\"full\"")).unwrap(),
            NetworkAccess::Full
        );
        assert!(NetworkAccess::read(&toml("\"some\"")).is_err());
        assert_eq!(
            NetworkAccess::Off.write(),
            Some(Value::String("off".to_owned()))
        );
    }

    #[test]
    fn a_rule_is_read_strictly_and_written_back() {
        let value = toml(r#"{ effect = "exec", match = "cargo test *", action = "allow" }"#);
        let rule = Rule::read(&value).unwrap();
        assert_eq!(rule.pattern, "cargo test *");
        assert_eq!(rule.write(), Some(value));

        for (wrong, what) in [
            (
                r#"{ effect = "exec", match = "x", action = "allow", until = "friday" }"#,
                "until",
            ),
            (
                r#"{ effect = "exec", match = "x", action = "permit" }"#,
                "permit",
            ),
            (
                r#"{ effect = "teleport", match = "x", action = "deny" }"#,
                "teleport",
            ),
            (r#"{ effect = "exec", action = "deny" }"#, "match"),
        ] {
            let error = Rule::read(&toml(wrong)).unwrap_err();
            assert!(
                error.contains("not a rule this version understands"),
                "{error}"
            );
            assert!(error.contains(what), "{what}: {error}");
        }
        assert!(Rule::read(&toml("\"allow all\"")).is_err());
    }

    #[test]
    fn text_can_name_an_environment_variable() {
        let literal = EnvText::read(&toml("\"abc\"")).unwrap();
        let named = EnvText::read(&toml("{ env = \"MY_TOKEN\" }")).unwrap();
        assert_eq!(literal, EnvText::Literal("abc".to_owned()));
        assert_eq!(named, EnvText::Env("MY_TOKEN".to_owned()));
        for wrong in [
            "5",
            "{ env = 5 }",
            "{ env = \"\" }",
            "{ env = \"A\", other = 1 }",
            "{ command = \"cat key\" }",
            "{}",
        ] {
            assert!(EnvText::read(&toml(wrong)).is_err(), "{wrong}");
        }

        let env = Env::empty(Platform::Unix).with("MY_TOKEN", "t0ken");
        assert_eq!(literal.resolve(&env).unwrap().expose(), "abc");
        assert_eq!(named.resolve(&env).unwrap().expose(), "t0ken");
        assert!(named.resolve(&Env::empty(Platform::Unix)).is_none());
        assert_eq!(named.write(), Some(toml("{ env = \"MY_TOKEN\" }")));

        let headers = Headers::read(&toml("{ X-One = \"1\", X-Two = { env = \"TWO\" } }")).unwrap();
        assert_eq!(headers.0.len(), 2);
        assert_eq!(Headers::read(&headers.write().unwrap()).unwrap(), headers);
        assert!(
            Headers::read(&toml("{ X-One = 1 }"))
                .unwrap_err()
                .starts_with("X-One:")
        );
        assert_eq!(Headers::default().write(), None);
    }

    #[test]
    fn a_choice_lists_its_words() {
        assert_eq!(
            StoreChoice::read(&toml("\"file\"")).unwrap(),
            StoreChoice::File
        );
        assert_eq!(
            StoreChoice::read(&toml("\"vault\"")).unwrap_err(),
            "expected one of `auto`, `file` or `keychain`"
        );
        assert_eq!(ReasoningVisibility::None.as_str(), "none");
        assert_eq!(one_of(&["only"]), "expected `only`");
    }

    #[test]
    fn sizes_compare_across_numbers_and_lengths_of_time() {
        assert_eq!(magnitude(&toml("5")), Some(5.0));
        assert_eq!(magnitude(&toml("2.5")), Some(2.5));
        assert_eq!(magnitude(&toml("\"2s\"")), Some(2000.0));
        assert_eq!(magnitude(&toml("\"soon\"")), None);
        assert_eq!(magnitude(&toml("true")), None);
    }
}
