//! `docs/dependencies.md` and the manifests agree (rule 7 of "Rules for every
//! step"): every direct third-party dependency has a row saying why it is
//! here, and every row is a dependency some crate really has.

use std::collections::{BTreeMap, BTreeSet};

use super::Violation;
use crate::workspace::Package;

pub(crate) const FILE: &str = "docs/dependencies.md";
const START: &str = "<!-- dependency-table:start -->";
const END: &str = "<!-- dependency-table:end -->";

/// A row of the table: where it is, and the crates it says use the dependency.
struct Row {
    line: usize,
    used_by: BTreeSet<String>,
}

pub(crate) fn check(packages: &[Package], list: &str) -> Vec<Violation> {
    let rows = match parse(list) {
        Ok(rows) => rows,
        Err(violation) => return vec![violation],
    };

    // Dependency -> the crates whose manifests name it, in any section.
    let mut declared: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut manifest_of: BTreeMap<&str, &str> = BTreeMap::new();
    for package in packages {
        for dependency in package.dependencies.iter().filter(|dep| !dep.in_workspace) {
            declared
                .entry(&dependency.name)
                .or_default()
                .insert(&package.name);
            manifest_of
                .entry(&dependency.name)
                .or_insert(&package.manifest);
        }
    }

    let mut violations = Vec::new();
    for (name, users) in &declared {
        let users_list = users.iter().copied().collect::<Vec<_>>().join(", ");
        match rows.get(*name) {
            None => violations.push(Violation::new(
                manifest_of[name],
                None,
                format!(
                    "`{name}` is a dependency of {users_list}, and {FILE} has no row for it.\n    \
                     No new dependency without an entry saying why it is here, what else was \
                     considered and what it costs (rule 7 in docs/build-plan.md)."
                ),
            )),
            Some(row)
                if !row
                    .used_by
                    .iter()
                    .map(String::as_str)
                    .eq(users.iter().copied()) =>
            {
                violations.push(Violation::new(
                    FILE,
                    Some(row.line),
                    format!(
                        "`{name}` is listed as used by {}, and the manifests have it in \
                         {users_list}.",
                        row.used_by.iter().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
            Some(_) => {}
        }
    }
    for (name, row) in &rows {
        if !declared.contains_key(name.as_str()) {
            violations.push(Violation::new(
                FILE,
                Some(row.line),
                format!("`{name}` is listed, and no crate depends on it. Remove the row."),
            ));
        }
    }
    violations
}

/// Reads the table between the two marker comments. The first cell of a row
/// is the dependency and the second the crates that use it, each name in
/// backticks.
fn parse(list: &str) -> Result<BTreeMap<String, Row>, Violation> {
    let missing = || {
        Violation::new(
            FILE,
            None,
            format!("the dependency table must sit between `{START}` and `{END}`."),
        )
    };
    let mut rows = BTreeMap::new();
    let mut inside = false;
    let mut closed = false;
    for (index, line) in list.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if trimmed == START {
            inside = true;
        } else if trimmed == END {
            closed = inside;
            inside = false;
        } else if inside && trimmed.starts_with('|') {
            let cells: Vec<&str> = trimmed
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            let names = names_in(cells.first().copied().unwrap_or(""));
            // The header and the separator row have no name in backticks.
            let Some(name) = names.first() else { continue };
            let used_by: BTreeSet<String> = names_in(cells.get(1).copied().unwrap_or(""))
                .into_iter()
                .collect();
            if used_by.is_empty() {
                return Err(Violation::new(
                    FILE,
                    Some(line_number),
                    format!("the row for `{name}` does not say which crate uses it."),
                ));
            }
            if rows
                .insert(
                    name.clone(),
                    Row {
                        line: line_number,
                        used_by,
                    },
                )
                .is_some()
            {
                return Err(Violation::new(
                    FILE,
                    Some(line_number),
                    format!("`{name}` has two rows."),
                ));
            }
        }
    }
    if closed { Ok(rows) } else { Err(missing()) }
}

/// The names written in backticks in a table cell.
fn names_in(cell: &str) -> Vec<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::check;
    use crate::workspace::{DepKind, Dependency, Package};

    fn krate(name: &str, third_party: &[&str]) -> Package {
        Package {
            name: name.to_owned(),
            manifest: format!("crates/{name}/Cargo.toml"),
            publish_disabled: true,
            dependencies: third_party
                .iter()
                .map(|dependency| Dependency {
                    name: (*dependency).to_owned(),
                    kind: DepKind::Normal,
                    in_workspace: false,
                    default_features: false,
                    features: Vec::new(),
                    optional: false,
                })
                .collect(),
            targets: Vec::new(),
            features: Vec::new(),
        }
    }

    fn table(rows: &[&str]) -> String {
        let mut text = String::from(
            "# Dependencies\n\n<!-- dependency-table:start -->\n| Dependency | Used by | Why |\n|---|---|---|\n",
        );
        for row in rows {
            text.push_str(row);
            text.push('\n');
        }
        text.push_str("<!-- dependency-table:end -->\n");
        text
    }

    fn messages(packages: &[Package], list: &str) -> Vec<String> {
        check(packages, list)
            .into_iter()
            .map(|violation| violation.to_string())
            .collect()
    }

    #[test]
    fn a_list_that_matches_the_manifests_passes() {
        let packages = [
            krate("cortex-config", &["toml"]),
            krate("cortex-knowledge", &["toml"]),
        ];
        let list =
            table(&["| `toml` | `cortex-config`, `cortex-knowledge` | Configuration files. |"]);

        assert_eq!(messages(&packages, &list), Vec::<String>::new());
    }

    #[test]
    fn a_dependency_with_no_row_fails() {
        let found = messages(&[krate("cortex-cli", &["lexopt"])], &table(&[]));

        assert_eq!(found.len(), 1);
        assert!(
            found[0].starts_with(
                "crates/cortex-cli/Cargo.toml: `lexopt` is a dependency of cortex-cli"
            )
        );
        assert!(found[0].contains("docs/dependencies.md has no row for it"));
    }

    #[test]
    fn a_row_for_a_dependency_nobody_has_fails() {
        let found = messages(
            &[krate("cortex-cli", &[])],
            &table(&["| `lexopt` | `cortex-cli` | Flags. |"]),
        );

        assert_eq!(
            found,
            [
                "docs/dependencies.md:6: `lexopt` is listed, and no crate depends on it. Remove the row."
            ]
        );
    }

    #[test]
    fn a_row_that_names_the_wrong_crates_fails() {
        let packages = [
            krate("cortex-config", &["toml"]),
            krate("cortex-knowledge", &["toml"]),
        ];
        let found = messages(
            &packages,
            &table(&["| `toml` | `cortex-config` | Configuration files. |"]),
        );

        assert_eq!(found.len(), 1);
        assert!(
            found[0]
                .starts_with("docs/dependencies.md:6: `toml` is listed as used by cortex-config,")
        );
        assert!(found[0].contains("the manifests have it in cortex-config, cortex-knowledge"));
    }

    #[test]
    fn a_missing_table_fails() {
        let found = messages(&[], "# Dependencies\n\nNone yet.\n");

        assert_eq!(found.len(), 1);
        assert!(found[0].contains("dependency-table:start"));
    }

    #[test]
    fn a_dependency_listed_twice_fails() {
        let list = table(&[
            "| `toml` | `cortex-config` | a |",
            "| `toml` | `cortex-config` | b |",
        ]);
        let found = messages(&[krate("cortex-config", &["toml"])], &list);

        assert_eq!(found, ["docs/dependencies.md:7: `toml` has two rows."]);
    }
}
