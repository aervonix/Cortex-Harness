//! Merging layers (`docs/architecture/08-configuration.md`, "Merge rules"
//! and "Trust and what a project may set").
//!
//! Layers are applied lowest priority first. For each setting a layer holds:
//!
//! 1. **A lock wins.** A key the managed file locks is not changed by any
//!    layer above the managed file.
//! 2. **A project's value passes the trust rules.** A harmless preference is
//!    applied. A value that can run code or widen access is applied only in
//!    a trusted project, and is otherwise held back and listed. A value that
//!    is the user's alone is ignored, with a warning.
//! 3. **Then the value is applied.** Tables merge key by key. A scalar or a
//!    list replaces what was there. The lists marked as joined in the
//!    schema (permission rules and the sandbox's path lists) are added to
//!    instead: their meaning does not depend on order, and a layer must not
//!    be able to drop a restriction by restating the list without it.
//!
//! Every applied value remembers where it came from.

use std::collections::BTreeMap;
use std::path::Path;

use toml::Value;

use crate::document::Document;
use crate::key::KeyPath;
use crate::layer::{Diagnostic, LayerKind, Origin, Severity, Withheld};
use crate::schema::{self, Meta, Node, Trust, TrustClass};

/// Where a table of settings being applied comes from.
pub(crate) struct Source<'a> {
    /// The layer the values count as.
    pub(crate) layer: LayerKind,
    /// The layer whose rights the values have: for a profile, the file that
    /// defines it.
    pub(crate) authority: LayerKind,
    /// The file, with its parsed form for line numbers.
    pub(crate) file: Option<(&'a Path, &'a Document)>,
    /// Where the table sits in the file: the root, or `profiles.<name>`.
    pub(crate) prefix: KeyPath,
    /// The profile's name, the environment variable or the flag.
    pub(crate) detail: Option<String>,
}

/// One layer as it was read, before locks and trust: what
/// `cortex config show --layer` prints.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LayerView {
    pub(crate) values: Vec<(KeyPath, Value, Origin)>,
}

/// The state of a merge in progress.
pub(crate) struct Merger {
    trusted: bool,
    /// The keys the managed file locks.
    pub(crate) locks: Vec<KeyPath>,
    defaults: toml::Table,
    /// Every value a layer set, merged.
    pub(crate) values: toml::Table,
    /// Where each set value came from.
    pub(crate) origins: BTreeMap<KeyPath, Origin>,
    /// Where each entry of each joined list came from.
    pub(crate) entry_origins: BTreeMap<KeyPath, Vec<Origin>>,
    /// Project settings held back until the project is trusted.
    pub(crate) withheld: Vec<Withheld>,
    /// What each layer said, as read.
    pub(crate) layers: BTreeMap<LayerKind, LayerView>,
    /// Warnings and notes.
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Merger {
    pub(crate) fn new(trusted: bool, locks: Vec<KeyPath>) -> Self {
        Self {
            trusted,
            locks,
            defaults: schema::defaults(),
            values: toml::Table::new(),
            origins: BTreeMap::new(),
            entry_origins: BTreeMap::new(),
            withheld: Vec::new(),
            layers: BTreeMap::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Applies a table of settings on top of what is merged so far.
    pub(crate) fn apply(&mut self, source: &Source<'_>, table: &toml::Table) {
        self.walk(source, schema::root(), table, &KeyPath::root());
    }

    fn walk(&mut self, source: &Source<'_>, node: &Node, table: &toml::Table, path: &KeyPath) {
        for (name, value) in table {
            // An unknown key was reported when the layer was checked.
            let Some(child) = node.child(name) else {
                continue;
            };
            // These describe the file. They are not settings to merge. The
            // locks are the exception, from the one file that may set them,
            // so that the effective configuration shows what is locked.
            if path.is_root()
                && (matches!(name.as_str(), "config_version" | "profiles")
                    || (name == "managed" && source.authority != LayerKind::Managed))
            {
                continue;
            }
            let key = path.child(name);
            match child.meta {
                Meta::Leaf { trust, joined, .. } => self.set(source, &key, value, trust, joined),
                Meta::Reserved { trust } => self.set_open(source, &key, value, trust),
                Meta::Nested { .. } => self.descend(source, &child.node, value, &key),
            }
        }
    }

    fn descend(&mut self, source: &Source<'_>, node: &Node, value: &Value, key: &KeyPath) {
        let Value::Table(table) = value else {
            return;
        };
        match node {
            Node::Table(_) => self.walk(source, node, table, key),
            Node::Map(inner) => {
                for (name, entry) in table {
                    self.descend(source, inner, entry, &key.child(name));
                }
            }
            Node::Leaf(_) | Node::Open | Node::Profiles => {}
        }
    }

    /// A reserved section has no declared keys, so it merges as plain TOML:
    /// tables key by key, everything else replaced.
    fn set_open(&mut self, source: &Source<'_>, key: &KeyPath, value: &Value, trust: Trust) {
        match value {
            Value::Table(table) => {
                for (name, entry) in table {
                    self.set_open(source, &key.child(name), entry, trust);
                }
            }
            _ => self.set(source, key, value, trust, false),
        }
    }

    fn set(
        &mut self,
        source: &Source<'_>,
        key: &KeyPath,
        value: &Value,
        trust: Trust,
        joined: bool,
    ) {
        if joined {
            let Value::Array(entries) = value else {
                return;
            };
            for (index, entry) in entries.iter().enumerate() {
                let origin = self.origin(source, key, Some(index));
                self.record(source, key, entry, &origin);
                if self.locked(source, key, &origin) {
                    continue;
                }
                let class = trust.class(entry, None);
                if self.admit(source, key, entry, class, &origin) {
                    push(&mut self.values, key, entry.clone());
                    self.entry_origins
                        .entry(key.clone())
                        .or_default()
                        .push(origin);
                }
            }
            return;
        }
        let origin = self.origin(source, key, None);
        self.record(source, key, value, &origin);
        if self.locked(source, key, &origin) {
            return;
        }
        let class = trust.class(value, self.current(key));
        if self.admit(source, key, value, class, &origin) {
            insert(&mut self.values, key, value.clone());
            self.origins.insert(key.clone(), origin);
        }
    }

    fn record(&mut self, source: &Source<'_>, key: &KeyPath, value: &Value, origin: &Origin) {
        self.layers.entry(source.layer).or_default().values.push((
            key.clone(),
            value.clone(),
            origin.clone(),
        ));
    }

    /// Whether a lock keeps this layer from changing the key. Says so once.
    fn locked(&mut self, source: &Source<'_>, key: &KeyPath, origin: &Origin) -> bool {
        if source.authority <= LayerKind::Managed
            || !self.locks.iter().any(|lock| key.is_at_or_under(lock))
        {
            return false;
        }
        self.diagnostics.push(Diagnostic {
            severity: Severity::Note,
            layer: Some(source.layer),
            file: origin.file.clone(),
            line: origin.line,
            key: Some(key.clone()),
            message: "locked by the managed configuration; this value is not used".to_owned(),
        });
        true
    }

    /// Whether the trust rules let this layer's value through.
    fn admit(
        &mut self,
        source: &Source<'_>,
        key: &KeyPath,
        value: &Value,
        class: TrustClass,
        origin: &Origin,
    ) -> bool {
        if !source.authority.is_project() {
            return true;
        }
        match class {
            TrustClass::Preference => true,
            TrustClass::UserOnly => {
                self.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    layer: Some(source.layer),
                    file: origin.file.clone(),
                    line: origin.line,
                    key: Some(key.clone()),
                    message: "a project's configuration may not set this; it is ignored. Set it \
                              in your own configuration file if you want it"
                        .to_owned(),
                });
                false
            }
            TrustClass::RunsCode | TrustClass::WidensAccess if self.trusted => true,
            TrustClass::RunsCode | TrustClass::WidensAccess => {
                self.withheld.push(Withheld {
                    key: key.clone(),
                    class,
                    value: value.clone(),
                    origin: origin.clone(),
                });
                false
            }
        }
    }

    /// The value in effect for a key so far: what a layer set, or the
    /// default.
    fn current(&self, key: &KeyPath) -> Option<&Value> {
        get(&self.values, key).or_else(|| get(&self.defaults, key))
    }

    fn origin(&self, source: &Source<'_>, key: &KeyPath, entry: Option<usize>) -> Origin {
        let place = source.prefix.join(key);
        Origin {
            layer: source.layer,
            defined_in: (source.layer != source.authority).then_some(source.authority),
            file: source.file.map(|(file, _)| file.to_path_buf()),
            line: source.file.and_then(|(_, document)| match entry {
                Some(index) => document.entry_line(&place, index),
                None => document.line_of(&place),
            }),
            detail: source.detail.clone(),
        }
    }
}

/// The value at a key path, when every table on the way exists.
pub(crate) fn get<'a>(table: &'a toml::Table, key: &KeyPath) -> Option<&'a Value> {
    let (last, tables) = key.parts().split_last()?;
    let mut table = table;
    for name in tables {
        table = table.get(name)?.as_table()?;
    }
    table.get(last)
}

/// Sets the value at a key path, creating the tables on the way. A value
/// that is in the way of a table is replaced by the table.
pub(crate) fn insert(table: &mut toml::Table, key: &KeyPath, value: Value) {
    let Some((last, tables)) = key.parts().split_last() else {
        return;
    };
    let mut table = table;
    for name in tables {
        let entry = table
            .entry(name.clone())
            .or_insert_with(|| Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = Value::Table(toml::Table::new());
        }
        let Value::Table(next) = entry else {
            return;
        };
        table = next;
    }
    table.insert(last.clone(), value);
}

/// Adds an entry to the list at a key path, creating the list.
fn push(table: &mut toml::Table, key: &KeyPath, entry: Value) {
    let mut entries = match get(table, key) {
        Some(Value::Array(entries)) => entries.clone(),
        _ => Vec::new(),
    };
    entries.push(entry);
    insert(table, key, Value::Array(entries));
}

/// Every setting in a table as a flat list, in key order. A table is gone
/// into unless the schema says it is one setting (a provider's headers).
pub(crate) fn flatten(table: &toml::Table) -> Vec<(KeyPath, &Value)> {
    fn go<'a>(table: &'a toml::Table, path: &KeyPath, flat: &mut Vec<(KeyPath, &'a Value)>) {
        for (name, value) in table {
            let key = path.child(name);
            match value {
                Value::Table(inner)
                    if !matches!(schema::lookup(&key), schema::Lookup::Setting { .. }) =>
                {
                    go(inner, &key, flat);
                }
                _ => flat.push((key, value)),
            }
        }
    }
    let mut flat = Vec::new();
    go(table, &KeyPath::root(), &mut flat);
    flat
}

/// One table laid over another: tables merge key by key, anything else in
/// `over` replaces what `under` has.
pub(crate) fn overlay(under: &toml::Table, over: &toml::Table) -> toml::Table {
    let mut merged = under.clone();
    for (name, value) in over {
        match (merged.get(name), value) {
            (Some(Value::Table(below)), Value::Table(above)) => {
                let table = overlay(below, above);
                merged.insert(name.clone(), Value::Table(table));
            }
            _ => {
                merged.insert(name.clone(), value.clone());
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> KeyPath {
        KeyPath::parse(text).unwrap()
    }

    fn table(text: &str) -> toml::Table {
        text.parse().unwrap()
    }

    #[test]
    fn values_are_read_and_set_by_path() {
        let mut values = table("[model]\nmain = \"a\"\n");
        assert_eq!(
            get(&values, &key("model.main")).and_then(Value::as_str),
            Some("a")
        );
        assert_eq!(get(&values, &key("model.fast")), None);
        assert_eq!(get(&values, &key("model.main.deeper")), None);
        assert_eq!(get(&values, &KeyPath::root()), None);

        insert(
            &mut values,
            &key("model.roles.fast.speed"),
            Value::String("fast".into()),
        );
        insert(&mut values, &key("model.main"), Value::String("b".into()));
        insert(&mut values, &key("top"), Value::Integer(1));
        // A value in the way of a table gives way.
        insert(&mut values, &key("top.inner"), Value::Integer(2));
        assert_eq!(
            values,
            table(
                "[model]\nmain = \"b\"\n[model.roles.fast]\nspeed = \"fast\"\n[top]\ninner = 2\n"
            )
        );

        push(
            &mut values,
            &key("sandbox.protect"),
            Value::String("a".into()),
        );
        push(
            &mut values,
            &key("sandbox.protect"),
            Value::String("b".into()),
        );
        assert_eq!(
            get(&values, &key("sandbox.protect")),
            Some(&Value::Array(vec![
                Value::String("a".into()),
                Value::String("b".into())
            ]))
        );
    }

    #[test]
    fn a_table_is_flattened_to_its_settings() {
        let values = table(
            "[model]\nmain = \"a\"\n[providers.x]\nheaders = { A = \"1\" }\nbase_url = \"u\"\n\
             [tui.colors]\naccent = \"blue\"\n",
        );
        let flat: Vec<String> = flatten(&values)
            .into_iter()
            .map(|(key, _)| key.to_string())
            .collect();
        assert_eq!(
            flat,
            [
                "model.main",
                "providers.x.base_url",
                "providers.x.headers",
                "tui.colors.accent"
            ],
            "a provider's headers are one setting; a reserved section is gone into"
        );
    }

    #[test]
    fn one_table_lies_over_another() {
        let under = table("a = 1\n[t]\nx = 1\ny = [1, 2]\n[u]\nk = 1\n");
        let over = table("a = 2\n[t]\ny = [3]\nz = true\nu = \"now a value\"\n");
        assert_eq!(
            overlay(&under, &over),
            table("a = 2\n[t]\nx = 1\ny = [3]\nz = true\nu = \"now a value\"\n[u]\nk = 1\n")
        );
    }
}
