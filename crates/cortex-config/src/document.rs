//! Reading one TOML file into values, keeping the line of every key.
//!
//! Layers are merged as plain values. What makes an error or an origin
//! useful is where it came from, so the line of each key, and of each entry
//! of each list, is kept beside the values.

use std::collections::BTreeMap;

use toml::Value;
use toml::de::{DeTable, DeValue};

use crate::key::KeyPath;

/// One parsed file.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Document {
    /// The file's content.
    pub(crate) table: toml::Table,
    /// The line each key is written on, counted from 1.
    pub(crate) lines: BTreeMap<KeyPath, u32>,
    /// The line of each entry of each list.
    pub(crate) entry_lines: BTreeMap<KeyPath, Vec<u32>>,
}

/// Why a file could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseFailure {
    /// What the parser said.
    pub(crate) message: String,
    /// The line it said it about.
    pub(crate) line: Option<u32>,
}

impl Document {
    /// The line of a key, or of the nearest table it is in.
    pub(crate) fn line_of(&self, key: &KeyPath) -> Option<u32> {
        let mut key = key.clone();
        loop {
            if let Some(line) = self.lines.get(&key) {
                return Some(*line);
            }
            key = key.parent()?;
        }
    }

    /// The line of one entry of a list, or failing that of the list.
    pub(crate) fn entry_line(&self, key: &KeyPath, index: usize) -> Option<u32> {
        self.entry_lines
            .get(key)
            .and_then(|lines| lines.get(index))
            .copied()
            .or_else(|| self.line_of(key))
    }
}

/// Parses the text of a configuration file. A byte-order mark at the start
/// is skipped, and either line ending is accepted.
pub(crate) fn parse(text: &str) -> Result<Document, ParseFailure> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines = LineIndex::new(text);
    let parsed = DeTable::parse(text).map_err(|error| ParseFailure {
        message: error.message().to_owned(),
        line: error.span().map(|span| lines.line(span.start)),
    })?;
    let mut document = Document::default();
    document.table = convert_table(
        parsed.get_ref(),
        &KeyPath::root(),
        &lines,
        &mut document.lines,
        &mut document.entry_lines,
    )?;
    Ok(document)
}

/// Finds the line of a byte offset.
struct LineIndex {
    /// The offset at which each line starts.
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(offset, _)| offset + 1),
        );
        Self { starts }
    }

    fn line(&self, offset: usize) -> u32 {
        let line = self.starts.partition_point(|start| *start <= offset);
        u32::try_from(line).unwrap_or(u32::MAX)
    }
}

fn convert_table(
    table: &DeTable<'_>,
    path: &KeyPath,
    index: &LineIndex,
    lines: &mut BTreeMap<KeyPath, u32>,
    entry_lines: &mut BTreeMap<KeyPath, Vec<u32>>,
) -> Result<toml::Table, ParseFailure> {
    let mut converted = toml::Table::new();
    for (name, value) in table {
        let key = path.child(name.get_ref());
        let line = index.line(name.span().start);
        lines.insert(key.clone(), line);
        let value = convert_value(value.get_ref(), &key, line, index, lines, entry_lines)?;
        converted.insert(name.get_ref().to_string(), value);
    }
    Ok(converted)
}

fn convert_value(
    value: &DeValue<'_>,
    key: &KeyPath,
    line: u32,
    index: &LineIndex,
    lines: &mut BTreeMap<KeyPath, u32>,
    entry_lines: &mut BTreeMap<KeyPath, Vec<u32>>,
) -> Result<Value, ParseFailure> {
    let out_of_range = |what: &str| ParseFailure {
        message: format!("`{key}`: the number {what} is too large"),
        line: Some(line),
    };
    Ok(match value {
        DeValue::String(text) => Value::String(text.to_string()),
        DeValue::Integer(number) => Value::Integer(
            i64::from_str_radix(number.as_str(), number.radix())
                .map_err(|_| out_of_range(number.as_str()))?,
        ),
        DeValue::Float(number) => Value::Float(
            number
                .as_str()
                .parse()
                .map_err(|_| out_of_range(number.as_str()))?,
        ),
        DeValue::Boolean(flag) => Value::Boolean(*flag),
        DeValue::Datetime(time) => Value::Datetime(*time),
        DeValue::Array(items) => {
            let mut converted = Vec::with_capacity(items.len());
            let mut item_lines = Vec::with_capacity(items.len());
            for item in items {
                let item_line = index.line(item.span().start);
                item_lines.push(item_line);
                // What is inside an entry is not looked up by key, so its
                // lines are not kept: the entry's own line is enough.
                let mut ignored_lines = BTreeMap::new();
                let mut ignored_entries = BTreeMap::new();
                converted.push(convert_value(
                    item.get_ref(),
                    key,
                    item_line,
                    index,
                    &mut ignored_lines,
                    &mut ignored_entries,
                )?);
            }
            entry_lines.insert(key.clone(), item_lines);
            Value::Array(converted)
        }
        DeValue::Table(table) => {
            Value::Table(convert_table(table, key, index, lines, entry_lines)?)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> KeyPath {
        KeyPath::parse(text).unwrap()
    }

    const FILE: &str = r#"config_version = 1
# A comment.
profile = "review"

[model]
main = "big-model"
speed = "fast"

[providers."my.server"]
base_url = "http://localhost:8080"
headers = { X-Team = "a" }

[[permissions.rules]]
effect = "exec"
match = "cargo test *"
action = "allow"

[[permissions.rules]]
effect = "write"
match = "**/.env*"
action = "deny"

[sandbox]
protect = [
    "secrets",
    "deploy/keys",
]
limits.turn.requests = 0x10
"#;

    #[test]
    fn every_key_keeps_its_line() {
        let document = parse(FILE).unwrap();
        for (path, line) in [
            ("config_version", 1),
            ("profile", 3),
            ("model.main", 6),
            ("model.speed", 7),
            (r#"providers."my.server".base_url"#, 10),
            (r#"providers."my.server".headers"#, 11),
            ("sandbox.protect", 24),
            ("sandbox.limits.turn.requests", 28),
        ] {
            assert_eq!(document.line_of(&key(path)), Some(line), "{path}");
        }
        // A key with no line of its own is placed at its table.
        assert_eq!(
            document.line_of(&key("model.missing")),
            document.line_of(&key("model"))
        );
        assert_eq!(document.line_of(&key("nowhere.at.all")), None);

        assert_eq!(document.table["model"]["main"].as_str(), Some("big-model"));
        assert_eq!(
            document.table["sandbox"]["limits"]["turn"]["requests"].as_integer(),
            Some(16)
        );
    }

    #[test]
    fn every_entry_of_a_list_keeps_its_line() {
        let document = parse(FILE).unwrap();
        let rules = key("permissions.rules");
        assert_eq!(document.entry_line(&rules, 0), Some(13));
        assert_eq!(document.entry_line(&rules, 1), Some(18));
        let protect = key("sandbox.protect");
        assert_eq!(document.entry_line(&protect, 0), Some(25));
        assert_eq!(document.entry_line(&protect, 1), Some(26));
        // Past the end, the list's own line.
        assert_eq!(document.entry_line(&protect, 9), Some(24));
        assert_eq!(
            document.table["permissions"]["rules"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_byte_order_mark_and_windows_line_endings_are_accepted() {
        let windows = format!("\u{feff}{}", FILE.replace('\n', "\r\n"));
        let document = parse(&windows).unwrap();
        assert_eq!(document.table, parse(FILE).unwrap().table);
        assert_eq!(document.line_of(&key("model.main")), Some(6));
        assert_eq!(document.entry_line(&key("permissions.rules"), 1), Some(18));
    }

    #[test]
    fn a_file_that_does_not_parse_says_where() {
        let failure = parse("[model]\nmain = \"x\"\nspeed = = 3\n").unwrap_err();
        assert_eq!(failure.line, Some(3), "{failure:?}");
        assert!(!failure.message.is_empty());

        let failure = parse("a = 1\na = 2\n").unwrap_err();
        assert_eq!(failure.line, Some(2), "{failure:?}");

        let failure = parse("big = 99999999999999999999\n").unwrap_err();
        assert_eq!(failure.line, Some(1), "{failure:?}");
        assert!(failure.message.contains("too large"), "{failure:?}");
    }

    #[test]
    fn an_empty_file_is_an_empty_document() {
        assert_eq!(parse("").unwrap(), Document::default());
        assert_eq!(
            parse("\u{feff}# only a comment\n").unwrap(),
            Document::default()
        );
    }
}
