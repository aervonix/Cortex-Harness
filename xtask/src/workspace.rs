//! What the commands need to know about the repository: where it is, which
//! packages it has (from `cargo metadata`), and which files it holds.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::Result;

/// The repository root: the directory that holds the workspace `Cargo.toml`.
pub(crate) fn root() -> PathBuf {
    // xtask's own manifest is at <root>/xtask/Cargo.toml.
    let xtask = Path::new(env!("CARGO_MANIFEST_DIR"));
    xtask.parent().unwrap_or(xtask).to_path_buf()
}

/// The cargo that is running this command, so every nested call uses the
/// same toolchain.
pub(crate) fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// The kind of a dependency edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DepKind {
    Normal,
    Dev,
    Build,
}

/// One dependency declared in a manifest.
#[derive(Clone, Debug)]
pub(crate) struct Dependency {
    /// The package name (not a rename).
    pub(crate) name: String,
    pub(crate) kind: DepKind,
    /// A path dependency inside this repository.
    pub(crate) in_workspace: bool,
    pub(crate) default_features: bool,
    pub(crate) features: Vec<String>,
}

/// One build target of a package.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub(crate) name: String,
    /// Cargo's target kinds: `lib`, `bin`, `test`, ...
    pub(crate) kinds: Vec<String>,
    pub(crate) doctest: bool,
}

/// One workspace package, reduced to what the rules read.
#[derive(Clone, Debug)]
pub(crate) struct Package {
    pub(crate) name: String,
    /// The manifest, relative to the repository root, with `/` separators.
    pub(crate) manifest: String,
    /// `publish = false`.
    pub(crate) publish_disabled: bool,
    pub(crate) dependencies: Vec<Dependency>,
    pub(crate) targets: Vec<Target>,
}

/// The workspace as `cargo metadata` describes it.
#[derive(Clone, Debug)]
pub(crate) struct Metadata {
    pub(crate) target_directory: PathBuf,
    pub(crate) packages: Vec<Package>,
}

/// Runs `cargo metadata` for the workspace. It reads manifests only: no
/// dependency resolution, no network.
pub(crate) fn metadata(root: &Path) -> Result<Metadata> {
    let output = Command::new(cargo())
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("could not run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata did not print JSON: {error}"))?;
    parse_metadata(&json)
}

fn parse_metadata(json: &Value) -> Result<Metadata> {
    let workspace_root = PathBuf::from(text(json, "workspace_root")?);
    let target_directory = PathBuf::from(text(json, "target_directory")?);
    let mut packages = Vec::new();
    for package in list(json, "packages")? {
        let manifest_path = PathBuf::from(text(package, "manifest_path")?);
        let manifest = manifest_path
            .strip_prefix(&workspace_root)
            .unwrap_or(&manifest_path)
            .to_string_lossy()
            .replace('\\', "/");

        let mut dependencies = Vec::new();
        for dependency in list(package, "dependencies")? {
            let kind = match dependency.get("kind").and_then(Value::as_str) {
                None => DepKind::Normal,
                Some("dev") => DepKind::Dev,
                Some("build") => DepKind::Build,
                Some(other) => return Err(format!("unknown dependency kind `{other}`")),
            };
            dependencies.push(Dependency {
                name: text(dependency, "name")?.to_owned(),
                kind,
                // Registry dependencies have a source; path dependencies have none.
                in_workspace: dependency.get("source").is_none_or(Value::is_null),
                default_features: dependency
                    .get("uses_default_features")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                features: strings(dependency, "features")?,
            });
        }

        let mut targets = Vec::new();
        for target in list(package, "targets")? {
            targets.push(Target {
                name: text(target, "name")?.to_owned(),
                kinds: strings(target, "kind")?,
                doctest: target
                    .get("doctest")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }

        packages.push(Package {
            name: text(package, "name")?.to_owned(),
            manifest,
            // `publish = false` is reported as an empty list of registries.
            publish_disabled: package
                .get("publish")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            dependencies,
            targets,
        });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Metadata {
        target_directory,
        packages,
    })
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("cargo metadata: `{key}` is missing or is not a string"))
}

fn list<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("cargo metadata: `{key}` is missing or is not a list"))
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>> {
    Ok(list(value, key)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// Every file that belongs to the repository, relative to its root, with `/`
/// separators: what git tracks plus new files it does not ignore.
///
/// Outside a git checkout (a copy of the tree made for a remote build, for
/// example) it falls back to walking the directory, skipping `.git` and
/// `target`.
pub(crate) fn repository_files(root: &Path) -> Result<Vec<String>> {
    let mut files = match git_files(root) {
        Some(files) => files,
        None => {
            let mut files = Vec::new();
            walk(root, Path::new(""), &mut files)?;
            files
        }
    };
    // A tracked file that was deleted and not yet committed is still listed.
    files.retain(|file| root.join(file).is_file());
    files.sort();
    Ok(files)
}

fn git_files(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8(output.stdout).ok()?;
    Some(
        listing
            .split('\0')
            .filter(|file| !file.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

fn walk(root: &Path, relative: &Path, files: &mut Vec<String>) -> Result<()> {
    let directory = root.join(relative);
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("could not list {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not list {}: {error}", directory.display()))?;
        let name = entry.file_name();
        let path = relative.join(&name);
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if file_type.is_dir() {
            if relative.as_os_str().is_empty() && (name == ".git" || name == "target") {
                continue;
            }
            walk(root, &path, files)?;
        } else if file_type.is_file() {
            files.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DepKind, parse_metadata};

    #[test]
    fn metadata_is_reduced_to_what_the_rules_read() {
        let json = json!({
            "workspace_root": "/repo",
            "target_directory": "/repo/target",
            "packages": [{
                "name": "cortex-agent",
                "manifest_path": "/repo/crates/cortex-agent/Cargo.toml",
                "publish": [],
                "dependencies": [
                    {
                        "name": "cortex-llm", "source": null, "kind": null,
                        "uses_default_features": false, "features": [],
                        "path": "/repo/crates/cortex-llm"
                    },
                    {
                        "name": "serde_json",
                        "source": "registry+https://github.com/rust-lang/crates.io-index",
                        "kind": "dev", "uses_default_features": true, "features": ["std"]
                    }
                ],
                "targets": [
                    { "name": "cortex_agent", "kind": ["lib"], "doctest": false },
                    { "name": "it", "kind": ["test"], "doctest": false }
                ]
            }]
        });

        let metadata = parse_metadata(&json).unwrap();
        let package = &metadata.packages[0];

        assert_eq!(package.name, "cortex-agent");
        assert_eq!(package.manifest, "crates/cortex-agent/Cargo.toml");
        assert!(package.publish_disabled);
        assert!(package.dependencies[0].in_workspace);
        assert!(!package.dependencies[0].default_features);
        assert_eq!(package.dependencies[0].kind, DepKind::Normal);
        assert!(!package.dependencies[1].in_workspace);
        assert_eq!(package.dependencies[1].kind, DepKind::Dev);
        assert_eq!(package.dependencies[1].features, ["std"]);
        assert_eq!(package.targets[1].kinds, ["test"]);
    }

    #[test]
    fn a_publishable_package_is_seen_as_one() {
        let json = json!({
            "workspace_root": "/repo",
            "target_directory": "/repo/target",
            "packages": [{
                "name": "cortex-base",
                "manifest_path": "/repo/crates/cortex-base/Cargo.toml",
                "publish": null,
                "dependencies": [],
                "targets": []
            }]
        });

        assert!(!parse_metadata(&json).unwrap().packages[0].publish_disabled);
    }
}
