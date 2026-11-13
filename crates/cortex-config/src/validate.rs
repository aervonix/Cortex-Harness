//! Checking one layer against the schema
//! (`docs/architecture/08-configuration.md`, "Validation").
//!
//! - **A wrong type or a value out of range is an error** that names the
//!   file, the line and the key.
//! - **An unknown key is a warning** with the nearest name that is known.
//!   Strict checking makes it an error.
//! - **In a table the harness enforces, an unknown key is always an error**
//!   (decision 0028). Permissions, the sandbox and the limits are read by
//!   more than one build: an older build that skipped a restriction it did
//!   not know would run with a wider configuration than the file asks for.
//!   So the file is refused whole, and the configuration that was in force
//!   stays.

use std::path::Path;

use toml::Value;

use crate::document::Document;
use crate::key::KeyPath;
use crate::layer::{Diagnostic, LayerKind, Severity};
use crate::schema::{self, CONFIG_VERSION, Child, Meta, Node};

/// The keys that belong to a file and so cannot be set by a profile.
const NOT_IN_A_PROFILE: [&str; 4] = ["config_version", "profile", "profiles", "managed"];

/// What a layer is checked as.
pub(crate) struct Checked<'a> {
    pub(crate) layer: LayerKind,
    pub(crate) file: Option<&'a Path>,
    pub(crate) document: &'a Document,
    /// Whether an unknown key is an error everywhere.
    pub(crate) strict: bool,
}

/// Checks a layer. Returns whether it can be used: `false` when an error
/// was added to `diagnostics`.
pub(crate) fn validate(checked: &Checked<'_>, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let before = errors(diagnostics);
    // The version first: the keys of a file written for a newer schema
    // cannot be judged by this one.
    if let Some(version) = checked.document.table.get("config_version") {
        let key = KeyPath::new(["config_version"]);
        match version.as_integer() {
            Some(found) if found > i64::from(CONFIG_VERSION) => {
                checked.report(
                    diagnostics,
                    Severity::Error,
                    &key,
                    format!(
                        "this file is written for configuration version {found}, and this \
                         version of Cortex-Harness understands up to {CONFIG_VERSION}. Update \
                         Cortex-Harness, or remove what the newer version added and set \
                         `config_version = {CONFIG_VERSION}`"
                    ),
                );
                return false;
            }
            Some(found) if found < 1 => {
                checked.report(
                    diagnostics,
                    Severity::Error,
                    &key,
                    format!("there is no configuration version {found}; the first is 1"),
                );
                return false;
            }
            // An older version would be migrated in memory here, with a
            // note. Version 1 is the first, so there is nothing to migrate.
            _ => {}
        }
    }
    check_table(
        checked,
        schema::root(),
        &checked.document.table,
        &KeyPath::root(),
        false,
        false,
        diagnostics,
    );
    errors(diagnostics) == before
}

fn errors(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .count()
}

impl Checked<'_> {
    fn report(
        &self,
        diagnostics: &mut Vec<Diagnostic>,
        severity: Severity,
        key: &KeyPath,
        message: String,
    ) {
        self.report_at(
            diagnostics,
            severity,
            key,
            self.document.line_of(key),
            message,
        );
    }

    fn report_at(
        &self,
        diagnostics: &mut Vec<Diagnostic>,
        severity: Severity,
        key: &KeyPath,
        line: Option<u32>,
        message: String,
    ) {
        diagnostics.push(Diagnostic {
            severity,
            layer: Some(self.layer),
            file: self.file.map(Path::to_path_buf),
            line,
            key: Some(key.clone()),
            message,
        });
    }
}

fn check_table(
    checked: &Checked<'_>,
    node: &Node,
    table: &toml::Table,
    path: &KeyPath,
    enforced: bool,
    in_profile: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (name, value) in table {
        let key = path.child(name);
        let Some(child) = node.child(name) else {
            unknown_key(checked, node, &key, name, enforced, diagnostics);
            continue;
        };
        if in_profile && path.len() == 2 && NOT_IN_A_PROFILE.contains(&name.as_str()) {
            checked.report(
                diagnostics,
                Severity::Error,
                &key,
                format!("`{name}` belongs to the file and cannot be set by a profile"),
            );
            continue;
        }
        if name == "managed" && path.is_root() && checked.layer != LayerKind::Managed {
            checked.report(
                diagnostics,
                Severity::Warning,
                &key,
                "only the managed configuration file may have a `[managed]` table; this one is \
                 ignored"
                    .to_owned(),
            );
            continue;
        }
        match child.meta {
            Meta::Leaf { range, .. } => check_leaf(checked, child, &key, value, range, diagnostics),
            Meta::Reserved { .. } => {
                if let Err(message) = (child.check)(value) {
                    checked.report(diagnostics, Severity::Error, &key, message);
                }
            }
            Meta::Nested { enforced: here } => check_nested(
                checked,
                &child.node,
                value,
                &key,
                enforced || here,
                in_profile,
                diagnostics,
            ),
        }
    }
}

fn check_nested(
    checked: &Checked<'_>,
    node: &Node,
    value: &Value,
    key: &KeyPath,
    enforced: bool,
    in_profile: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Value::Table(table) = value else {
        checked.report(
            diagnostics,
            Severity::Error,
            key,
            "expected a table".to_owned(),
        );
        return;
    };
    match node {
        Node::Table(_) => check_table(checked, node, table, key, enforced, in_profile, diagnostics),
        Node::Map(inner) => {
            for (name, entry) in table {
                check_nested(
                    checked,
                    inner,
                    entry,
                    &key.child(name),
                    enforced,
                    in_profile,
                    diagnostics,
                );
            }
        }
        Node::Profiles => {
            if in_profile {
                // Reached only through a key that is refused before this.
                return;
            }
            for (name, entry) in table {
                let profile = key.child(name);
                match entry {
                    Value::Table(settings) => check_table(
                        checked,
                        schema::root(),
                        settings,
                        &profile,
                        false,
                        true,
                        diagnostics,
                    ),
                    _ => checked.report(
                        diagnostics,
                        Severity::Error,
                        &profile,
                        "expected a table of settings".to_owned(),
                    ),
                }
            }
        }
        Node::Leaf(_) | Node::Open => {}
    }
}

fn check_leaf(
    checked: &Checked<'_>,
    child: &Child,
    key: &KeyPath,
    value: &Value,
    range: Option<(f64, f64)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Err(message) = (child.check)(value) {
        // A list says which entry was wrong: "entry 2: …". The error is
        // then placed on that entry's line.
        let line = message
            .strip_prefix("entry ")
            .and_then(|rest| rest.split(':').next())
            .and_then(|number| number.parse::<usize>().ok())
            .and_then(|number| checked.document.entry_line(key, number.checked_sub(1)?))
            .or_else(|| checked.document.line_of(key));
        checked.report_at(diagnostics, Severity::Error, key, line, message);
        return;
    }
    let number = match value {
        Value::Integer(number) => Some(*number as f64),
        Value::Float(number) => Some(*number),
        _ => None,
    };
    if let (Some((low, high)), Some(number)) = (range, number)
        && !(low..=high).contains(&number)
    {
        checked.report(
            diagnostics,
            Severity::Error,
            key,
            format!("{number} is out of range: expected {low} to {high}"),
        );
    }
}

fn unknown_key(
    checked: &Checked<'_>,
    node: &Node,
    key: &KeyPath,
    name: &str,
    enforced: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let suggestion = nearest(name, &schema::names(node))
        .map(|known| format!(" Did you mean `{known}`?"))
        .unwrap_or_default();
    let (severity, message) = if enforced {
        (
            Severity::Error,
            format!(
                "unknown key in a table that is enforced.{suggestion} A permission, sandbox or \
                 limit setting that this version does not understand cannot be applied, and is \
                 never skipped, so the file is refused"
            ),
        )
    } else if checked.strict {
        (Severity::Error, format!("unknown key.{suggestion}"))
    } else {
        (
            Severity::Warning,
            format!("unknown key, ignored.{suggestion}"),
        )
    };
    checked.report(diagnostics, severity, key, message);
}

/// The known name closest to `name`, when one is close enough to be what
/// was meant.
pub(crate) fn nearest<'a>(name: &str, known: &[&'a str]) -> Option<&'a str> {
    // How many slips are allowed grows with the length: none in a name of
    // one or two letters, where anything is near anything.
    let limit = match name.chars().count() {
        0..=2 => 0,
        3..=5 => 2,
        _ => 3,
    };
    known
        .iter()
        .map(|candidate| (distance(name, candidate), *candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// The number of single-character insertions, deletions and substitutions
/// that turn one word into the other.
fn distance(first: &str, second: &str) -> usize {
    let second: Vec<char> = second.chars().collect();
    let mut row: Vec<usize> = (0..=second.len()).collect();
    for (i, a) in first.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, b) in second.iter().enumerate() {
            let substitute = previous + usize::from(a != *b);
            previous = row[j + 1];
            row[j + 1] = substitute.min(previous + 1).min(row[j] + 1);
        }
    }
    row[second.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse;

    fn check(layer: LayerKind, strict: bool, text: &str) -> (bool, Vec<String>) {
        let document = parse(text).unwrap();
        let mut diagnostics = Vec::new();
        let usable = validate(
            &Checked {
                layer,
                file: Some(Path::new("/work/config.toml")),
                document: &document,
                strict,
            },
            &mut diagnostics,
        );
        let env = crate::env::Env::empty(crate::env::Platform::Unix);
        (usable, diagnostics.iter().map(|d| d.render(&env)).collect())
    }

    fn user(text: &str) -> (bool, Vec<String>) {
        check(LayerKind::User, false, text)
    }

    #[test]
    fn a_file_that_is_right_has_nothing_to_report() {
        let (usable, said) = user(
            r#"
            config_version = 1
            profile = "review"
            [model]
            main = "big"
            speed = "fast"
            speed_wait = "5s"
            [model.roles.fast]
            speed = "standard"
            [providers.local]
            base_url = "http://localhost:1"
            headers = { X = { env = "TOKEN" } }
            [retry]
            attempts = 3
            backoff_factor = 1.5
            [[permissions.rules]]
            effect = "exec"
            match = "cargo test *"
            action = "allow"
            [sandbox]
            protect = ["secrets"]
            [limits.day]
            cost = 20
            [profiles.review]
            model.main = "other"
            permissions.mode = "read-only"
            [tui]
            anything = { goes = ["here", 1] }
            "#,
        );
        assert!(usable, "{said:?}");
        assert!(said.is_empty(), "{said:?}");
    }

    #[test]
    fn a_wrong_type_names_the_file_the_line_and_the_key() {
        let (usable, said) = user("[retry]\nattempts = \"five\"\nbackoff_cap = 30\n");
        assert!(!usable);
        assert_eq!(
            said,
            [
                "error: /work/config.toml:2: retry.attempts: expected a whole number that is \
                 not negative",
                "error: /work/config.toml:3: retry.backoff_cap: expected a length of time such \
                 as \"20s\", \"10m\" or \"1h30m\" (units: ms, s, m, h, d)",
            ]
        );
    }

    #[test]
    fn a_number_out_of_range_is_an_error() {
        let (usable, said) = user("[retry]\nattempts = 0\nbackoff_factor = 0.5\n");
        assert!(!usable);
        assert_eq!(
            said,
            [
                "error: /work/config.toml:2: retry.attempts: 0 is out of range: expected 1 to 20",
                "error: /work/config.toml:3: retry.backoff_factor: 0.5 is out of range: \
                 expected 1 to 10",
            ]
        );
    }

    #[test]
    fn an_unknown_key_is_a_warning_with_the_nearest_name() {
        let (usable, said) = user("[model]\nmian = \"x\"\n[retyr]\nattempts = 2\n[zzz]\n");
        assert!(usable, "a warning does not stop the file being used");
        assert_eq!(
            said,
            [
                "warning: /work/config.toml:2: model.mian: unknown key, ignored. Did you mean \
                 `main`?",
                "warning: /work/config.toml:3: retyr: unknown key, ignored. Did you mean `retry`?",
                "warning: /work/config.toml:5: zzz: unknown key, ignored.",
            ]
        );

        let (usable, said) = check(LayerKind::User, true, "[model]\nmian = \"x\"\n");
        assert!(!usable, "strict checking makes it an error");
        assert_eq!(
            said,
            ["error: /work/config.toml:2: model.mian: unknown key. Did you mean `main`?"]
        );
    }

    #[test]
    fn an_unknown_key_in_an_enforced_table_refuses_the_file() {
        for text in [
            "[permissions]\nmode = \"supervised\"\nquarantine = true\n",
            "[sandbox]\nread = [\"/srv\"]\n",
            "[limits.session]\nfiles = 10\n",
            "[limits.week]\ncost = 5\n",
            "[profiles.tight.permissions]\nnew_restriction = 1\n",
        ] {
            let (usable, said) = user(text);
            assert!(!usable, "{text}");
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(
                said[0].starts_with("error: ") && said[0].contains("is refused"),
                "{said:?}"
            );
        }
        // A value this version does not know is refused the same way.
        let (usable, said) = user("[permissions]\nmode = \"cautious\"\n");
        assert!(!usable);
        assert!(said[0].contains("expected one of `read-only`"), "{said:?}");
        let (usable, said) = user(
            "[[permissions.rules]]\neffect = \"exec\"\nmatch = \"x\"\naction = \"allow\"\n\n\
             [[permissions.rules]]\neffect = \"exec\"\nmatch = \"y\"\naction = \"allow\"\n\
             until = \"friday\"\n",
        );
        assert!(!usable);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with("error: /work/config.toml:6: permissions.rules: entry 2:"),
            "the error is on the second rule's line: {said:?}"
        );
    }

    #[test]
    fn a_newer_file_is_refused_with_one_clear_message() {
        let (usable, said) = user("config_version = 2\n[model]\nfuture_key = 1\n");
        assert!(!usable);
        assert_eq!(
            said.len(),
            1,
            "nothing else in the file is judged: {said:?}"
        );
        assert!(
            said[0].contains("written for configuration version 2")
                && said[0].contains("understands up to 1"),
            "{said:?}"
        );
        let (usable, said) = user("config_version = 0\n");
        assert!(!usable);
        assert!(
            said[0].contains("there is no configuration version 0"),
            "{said:?}"
        );
    }

    #[test]
    fn a_profile_holds_settings_and_not_the_keys_of_the_file() {
        let (usable, said) = user(
            "[profiles.p]\nprofile = \"q\"\nconfig_version = 1\nmodel.mian = \"x\"\n\
             retry.attempts = 99\n[profiles.p.managed]\nlocked = []\n[profiles.p.profiles.q]\n",
        );
        assert!(!usable);
        let text = said.join("\n");
        for expected in [
            "profiles.p.profile: `profile` belongs to the file",
            "profiles.p.config_version: `config_version` belongs to the file",
            "profiles.p.managed: `managed` belongs to the file",
            "profiles.p.profiles: `profiles` belongs to the file",
            "warning: /work/config.toml:4: profiles.p.model.mian: unknown key, ignored. Did you \
             mean `main`?",
            "profiles.p.retry.attempts: 99 is out of range",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
        let (usable, said) = user("[profiles]\nquick = 3\n");
        assert!(!usable);
        assert!(
            said[0].contains("profiles.quick: expected a table of settings"),
            "{said:?}"
        );
    }

    #[test]
    fn only_the_managed_file_has_a_managed_table() {
        let text = "[managed]\nlocked = [\"permissions.mode\"]\n";
        let (usable, said) = user(text);
        assert!(usable);
        assert_eq!(said.len(), 1);
        assert!(
            said[0].starts_with("warning: ") && said[0].contains("is ignored"),
            "{said:?}"
        );
        let (usable, said) = check(LayerKind::Managed, false, text);
        assert!(usable && said.is_empty(), "{said:?}");
    }

    #[test]
    fn a_table_where_a_value_belongs_and_a_value_where_a_table_belongs() {
        let (usable, said) =
            user("model = \"big\"\n[retry.attempts]\nx = 1\n[providers]\na = 1\ntui = 5\n");
        assert!(!usable);
        let text = said.join("\n");
        assert!(text.contains("model: expected a table"), "{text}");
        assert!(
            text.contains("retry.attempts: expected a whole number"),
            "{text}"
        );
        assert!(text.contains("providers.a: expected a table"), "{text}");
    }

    #[test]
    fn the_nearest_name_must_be_near() {
        let known = ["main", "fast", "judge", "effort", "speed_fallback"];
        assert_eq!(nearest("mian", &known), Some("main"));
        assert_eq!(nearest("judg", &known), Some("judge"));
        assert_eq!(nearest("speed_falback", &known), Some("speed_fallback"));
        assert_eq!(nearest("temperature", &known), None);
        assert_eq!(nearest("x", &known), None);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("same", "same"), 0);
    }
}
