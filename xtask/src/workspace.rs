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

/// Where a run of nextest in `profile` leaves its JUnit report, for the
/// workspace at `root`.
///
/// nextest keeps its files in `target/nextest` under the workspace root
/// (its `store.dir` setting), and not in cargo's target directory: a build
/// that is sent elsewhere with `CARGO_TARGET_DIR` still reports here.
pub(crate) fn nextest_report(root: &Path, profile: &str) -> PathBuf {
    root.join("target")
        .join("nextest")
        .join(profile)
        .join("junit.xml")
}

/// Removes the report of an earlier run, so that a run that fails before it
/// writes one is not followed by a reading of the last run's.
pub(crate) fn forget_report(report: &Path) -> Result<()> {
    match fs::remove_file(report) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", report.display())),
    }
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
    /// Only built when a feature of the package asks for it.
    pub(crate) optional: bool,
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
    /// The package's own features, each with what it turns on, in name
    /// order: `("default", ["std"])`.
    pub(crate) features: Vec<(String, Vec<String>)>,
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
                optional: dependency
                    .get("optional")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
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

        let mut features: Vec<(String, Vec<String>)> = package
            .get("features")
            .and_then(Value::as_object)
            .map(|features| {
                features
                    .iter()
                    .map(|(name, turns_on)| {
                        let turns_on = turns_on.as_array().map_or_else(Vec::new, |list| {
                            list.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        });
                        (name.clone(), turns_on)
                    })
                    .collect()
            })
            .unwrap_or_default();
        features.sort();

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
            features,
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
///
/// Git is asked only when `root` is the top of a checkout. A directory
/// inside one that git ignores (a copy of the tree under `target/`) is not a
/// repository of its own: git would answer for the checkout around it, and
/// say that the directory holds no file at all.
pub(crate) fn repository_files(root: &Path) -> Result<Vec<String>> {
    let checkout = root.join(".git").exists();
    let mut files = match checkout.then(|| git_files(root)).flatten() {
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
                ],
                "features": { "test-hooks": [], "default": ["std"] }
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
        assert_eq!(
            package.features,
            [
                ("default".to_owned(), vec!["std".to_owned()]),
                ("test-hooks".to_owned(), vec![])
            ]
        );
    }

    /// The name starts with `sys_` because it starts git.
    #[test]
    fn sys_a_directory_that_a_checkout_ignores_is_listed_by_walking_it() {
        let outer = std::env::temp_dir().join(format!("cortex-xtask-files-{}", std::process::id()));
        if outer.exists() {
            std::fs::remove_dir_all(&outer).unwrap();
        }
        let copy = outer.join("target").join("copy");
        std::fs::create_dir_all(copy.join("src")).unwrap();
        std::fs::write(outer.join(".gitignore"), "/target/\n").unwrap();
        std::fs::write(outer.join("kept.txt"), "tracked").unwrap();
        std::fs::write(copy.join("src").join("lib.rs"), "// a copy").unwrap();
        std::fs::write(copy.join("README.md"), "# A copy").unwrap();
        let initialised = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&outer)
            .status()
            .unwrap();
        assert!(initialised.success());

        // The checkout itself: what git does not ignore.
        assert_eq!(
            super::repository_files(&outer).unwrap(),
            [".gitignore", "kept.txt"]
        );
        // The copy inside it. Git, asked there, lists nothing.
        assert_eq!(
            super::repository_files(&copy).unwrap(),
            ["README.md", "src/lib.rs"]
        );

        std::fs::remove_dir_all(&outer).unwrap();
    }

    #[test]
    fn an_old_report_is_removed_and_no_report_is_no_error() {
        let directory =
            std::env::temp_dir().join(format!("cortex-xtask-report-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let report = directory.join("junit.xml");
        std::fs::write(&report, "<testsuites/>").unwrap();

        super::forget_report(&report).unwrap();
        assert!(!report.exists());
        super::forget_report(&report).unwrap();

        std::fs::remove_dir_all(&directory).unwrap();
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
