//! The repo-rule checks: the part of tier 0 that no off-the-shelf tool does
//! (`docs/testing-and-ci.md`).
//!
//! Each rule is a function from plain inputs (file contents, the package
//! list) to a list of violations, so it can be tested without a repository.
//! [`check_repository`] feeds them the real thing.

use std::fmt;
use std::fs;
use std::path::Path;

use crate::Result;
use crate::workspace;

mod crate_graph;
mod dependency_list;
mod home_paths;
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

    let files = workspace::repository_files(root)?;
    for file in &files {
        let path = root.join(file);
        let bytes = fs::read(&path).map_err(|error| format!("could not read {file}: {error}"))?;
        violations.extend(home_paths::check(file, &bytes));
        if file.starts_with(".github/workflows/") {
            violations.extend(workflows::check(file, &String::from_utf8_lossy(&bytes)));
        }
    }

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
