//! The repo-rule checks: the part of tier 0 that no off-the-shelf tool does
//! (`docs/testing-and-ci.md`).
//!
//! Each rule is a function from plain inputs (file contents, the package
//! list) to a list of violations, so it can be tested without a repository.
//! [`check_repository`] feeds them the real thing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use crate::Result;
use crate::workspace;

mod crate_graph;
mod dependency_list;
mod home_paths;
mod layout;
mod links;
mod rust_source;
mod workflows;

/// One broken rule, at a place in the repository.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    /// The file, relative to the repository root.
    pub(crate) file: String,
    pub(crate) line: Option<usize>,
    /// What is wrong and how to put it right. May span several lines.
    pub(crate) message: String,
}

impl Violation {
    fn new(file: impl Into<String>, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: {}", self.file, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

/// Runs every rule against the repository at `root`.
pub(crate) fn check_repository(root: &Path) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();

    // What a pull request holds; a link may lead to a new file too.
    let files = workspace::tracked_files(root)?;
    let names = workspace::repository_files(root)?;
    let mut sources = BTreeMap::new();
    for file in &files {
        let path = root.join(file);
        let bytes = fs::read(&path).map_err(|error| format!("could not read {file}: {error}"))?;
        violations.extend(home_paths::check(file, &bytes));
        if file.starts_with(".github/workflows/") {
            violations.extend(workflows::check(file, &String::from_utf8_lossy(&bytes)));
        }
        if rust_source::is_read(file) {
            sources.insert(file.clone(), String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    violations.extend(rust_source::check(&sources));
    violations.extend(check_docs(root, &files, &names)?);

    let metadata = workspace::metadata(root)?;
    let manifests = metadata
        .packages
        .iter()
        .map(|package| {
            fs::read_to_string(root.join(&package.manifest))
                .map(|text| (package.manifest.clone(), text))
                .map_err(|error| format!("could not read {}: {error}", package.manifest))
        })
        .collect::<Result<Vec<_>>>()?;
    violations.extend(crate_graph::check(&metadata.packages, &manifests));

    let list = fs::read_to_string(root.join(dependency_list::FILE))
        .map_err(|error| format!("could not read {}: {error}", dependency_list::FILE))?;
    violations.extend(dependency_list::check(&metadata.packages, &list));

    Ok(violations)
}

/// The rules about the docs alone: every link inside the repository leads
/// somewhere, and the step records and open items are where they belong.
/// They read text files and nothing else, so `cargo xtask links` runs them
/// without asking cargo anything.
///
/// `files` are read; a link may lead to any of `names`, which holds them.
pub(crate) fn check_docs(
    root: &Path,
    files: &[String],
    names: &[String],
) -> Result<Vec<Violation>> {
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    for file in files {
        if links::is_read(file) || layout::is_read(file) {
            let bytes = fs::read(root.join(file))
                .map_err(|error| format!("could not read {file}: {error}"))?;
            texts.insert(file.clone(), String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    let names: BTreeSet<String> = names.iter().chain(files).cloned().collect();
    let mut violations = links::check(&texts, &names);
    violations.extend(layout::check(&texts));
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::check_repository;
    use crate::workspace;

    /// The repository itself follows its rules. This is the same check as
    /// `cargo xtask rules`, run as a test so that it also runs on Windows.
    /// The name starts with `sys_` because it starts cargo and git.
    #[test]
    fn sys_the_repository_follows_its_own_rules() {
        let violations = check_repository(&workspace::root()).unwrap();
        let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
        assert!(violations.is_empty(), "\n{}\n", report.join("\n"));
    }
}
