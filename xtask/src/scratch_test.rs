//! `cargo xtask scratch-test CRATE FILE`: one throwaway test file run
//! against a crate's integration tests, with nothing to register by hand
//! and nothing left behind (`docs/testing-and-ci.md`, "A throwaway test").
//!
//! A review that confirms a finding writes a failing test for it. Without
//! this it had to add the file to `tests/it/` and a `mod` line to its
//! `main.rs`, and take both out again: a step a killed run, or a commit
//! made in a hurry, gets wrong.
//!
//! **The worktree is only read.** The run is made in a copy of the tree
//! under `target/scratch-test/tree`, which git ignores, with a build
//! directory of its own (`target/scratch-test/target`):
//!
//! 1. the copy is brought up to date with the worktree: what git tracks and
//!    what it does not ignore, as `mutate`'s copies are. Whatever an
//!    earlier run added to the copy, a run that was killed included, goes
//!    here;
//! 2. the file is put beside the crate's integration tests in the copy, as
//!    a module named `scratch_<its name>`, and `main.rs` there gets its
//!    `mod` line;
//! 3. nextest runs that module's tests alone, in the copy;
//! 4. the copy is brought up to date again, which takes the module out.
//!
//! The first run builds the crate and what it depends on in the copy's
//! build directory; later runs rebuild what changed. One run at a time: a
//! second waits for nothing and is refused while the first holds the lock.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::workspace;

const USAGE: &str = "\
usage: cargo xtask scratch-test CRATE FILE [-- NEXTEST ARGUMENTS]

Runs the tests in FILE, a Rust file of #[test] functions written as a module
of CRATE's integration tests (`use crate::…` reaches the binary's helpers), in
a copy of the tree under target/scratch-test/. Nothing is written to the
worktree. It fails when a test fails, and when FILE has no test that ran.";

/// Where the copy and its build are, under the worktree's build directory.
fn scratch_dir(root: &Path) -> PathBuf {
    root.join("target").join("scratch-test")
}

/// What was asked.
#[derive(Debug, PartialEq)]
struct Request {
    package: String,
    file: PathBuf,
    rest: Vec<String>,
}

impl Request {
    fn parse(arguments: &[String]) -> Result<Self> {
        let (ours, rest) = match arguments.iter().position(|word| word == "--") {
            Some(at) => (&arguments[..at], arguments[at + 1..].to_vec()),
            None => (arguments, Vec::new()),
        };
        match ours {
            [package, file] if !package.starts_with('-') && !file.starts_with('-') => Ok(Self {
                package: package.clone(),
                file: PathBuf::from(file),
                rest,
            }),
            _ => Err(USAGE.to_owned()),
        }
    }
}

/// The module the file becomes: `scratch_` and its name, made a Rust name.
fn module_name(file: &Path) -> Result<String> {
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("{} has no name to make a module of", file.display()))?;
    let name: String = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    Ok(format!("scratch_{name}"))
}

/// Puts `source` in the copy at `copy` as module `module` of `package`'s
/// integration tests. Returns the test filter that runs it alone.
fn place(copy: &Path, package: &str, module: &str, source: &str) -> Result<String> {
    let tests = copy.join("crates").join(package).join("tests").join("it");
    let main = tests.join("main.rs");
    let mut text = fs::read_to_string(&main).map_err(|_| {
        format!(
            "{package} has no integration tests at crates/{package}/tests/it/main.rs to run a \
             scratch test beside"
        )
    })?;
    let file = tests.join(format!("{module}.rs"));
    if file.exists() {
        return Err(format!(
            "crates/{package}/tests/it/{module}.rs is a file of the crate's own: name the \
             scratch file something else"
        ));
    }
    fs::write(&file, source).map_err(|error| format!("could not write the module: {error}"))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "\n// Added by `cargo xtask scratch-test`, in its copy of the tree only.\nmod {module};\n"
    ));
    fs::write(&main, text).map_err(|error| format!("could not write main.rs: {error}"))?;
    Ok(format!("test(/^{module}::/)"))
}

/// Takes the copy for this run. The lock is the system's, so a run that is
/// killed lets go of it.
fn lock(scratch: &Path) -> Result<File> {
    fs::create_dir_all(scratch)
        .map_err(|error| format!("could not create {}: {error}", scratch.display()))?;
    let path = scratch.join("lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(
            "another `cargo xtask scratch-test` is running on the copy: wait for it to end"
                .to_owned(),
        ),
        Err(fs::TryLockError::Error(error)) => {
            Err(format!("could not lock {}: {error}", path.display()))
        }
    }
}

/// Runs `request` in a copy of the tree at `root`, with `run` doing the
/// testing: the copy, the filter and the extra arguments. The copy is made
/// like the worktree before the run and again after it, whatever the run
/// did.
fn scratch_test(
    root: &Path,
    request: &Request,
    run: impl FnOnce(&Path, &str, &[String]) -> Result<()>,
) -> Result<()> {
    let source = fs::read_to_string(&request.file)
        .map_err(|error| format!("could not read {}: {error}", request.file.display()))?;
    let module = module_name(&request.file)?;
    let scratch = scratch_dir(root);
    let _lock = lock(&scratch)?;
    let copy = scratch.join("tree");
    let files = crate::mutate::tree_files(root)?;
    crate::mutate::sync(root, &copy, &files)?;
    let ran = place(&copy, &request.package, &module, &source)
        .and_then(|filter| run(&copy, &filter, &request.rest));
    // The module goes again, pass or fail.
    let tidied = crate::mutate::sync(root, &copy, &files);
    ran.and(tidied)
}

/// `cargo xtask scratch-test …`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let request = Request::parse(arguments)?;
    crate::checks::require_nextest()?;
    let root = workspace::root();
    let target = scratch_dir(&root).join("target");
    scratch_test(&root, &request, |copy, filter, rest| {
        let mut command = vec![
            "nextest".to_owned(),
            "run".to_owned(),
            "--locked".to_owned(),
            "-p".to_owned(),
            request.package.clone(),
        ];
        command.extend(workspace::implied_features(std::slice::from_ref(
            &request.package,
        )));
        command.extend(["-E".to_owned(), filter.to_owned()]);
        command.extend(rest.iter().cloned());
        println!(
            "$ cargo {}   (in target/scratch-test/tree, building in target/scratch-test/target)",
            command.join(" ")
        );
        let status = Command::new(workspace::cargo())
            .args(&command)
            .current_dir(copy)
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .map_err(|error| format!("could not run cargo: {error}"))?;
        match status.code() {
            Some(0) => Ok(()),
            // nextest's "no tests to run": a file with no test in it would
            // otherwise pass.
            Some(4) => Err(format!(
                "no test of {} ran: it has no #[test] function, or the arguments after `--` \
                 filtered them all out",
                request.file.display()
            )),
            _ => Err(format!("a test of {} failed", request.file.display())),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{Request, module_name, scratch_test};

    /// A tree with git's files, as a worktree has them: a crate with an
    /// integration-test binary.
    fn tree(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("xtask-scratch-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let it = root.join("crates/demo/tests/it");
        fs::create_dir_all(&it).unwrap();
        fs::write(it.join("main.rs"), "mod one;\n").unwrap();
        fs::write(it.join("one.rs"), "#[test]\nfn one() {}\n").unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        root
    }

    /// Every file under `dir` with its content, but the build directory.
    fn contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if relative == "target" {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, base, out);
                } else {
                    out.push((relative, fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn the_file_runs_in_a_copy_and_nothing_is_left_behind() {
        let root = tree("left");
        let outside = root.with_extension("files");
        fs::create_dir_all(&outside).unwrap();
        let file = outside.join("finding.rs");
        fs::write(&file, "#[test]\nfn the_finding() { assert!(false); }\n").unwrap();
        let before = contents(&root);
        let request = Request {
            package: "demo".to_owned(),
            file: file.clone(),
            rest: vec!["--no-capture".to_owned()],
        };
        let mut seen = None;
        let outcome = scratch_test(&root, &request, |copy, filter, rest| {
            let it = copy.join("crates/demo/tests/it");
            seen = Some((
                fs::read_to_string(it.join("main.rs")).unwrap(),
                fs::read_to_string(it.join("scratch_finding.rs")).unwrap(),
                filter.to_owned(),
                rest.to_vec(),
            ));
            Err("a test failed".to_owned())
        });
        // The run's own failure is the command's.
        assert_eq!(outcome, Err("a test failed".to_owned()));
        let (main, module, filter, rest) = seen.unwrap();
        assert!(main.starts_with("mod one;\n") && main.ends_with("mod scratch_finding;\n"));
        assert!(module.contains("fn the_finding"));
        assert_eq!(filter, "test(/^scratch_finding::/)");
        assert_eq!(rest, ["--no-capture"]);
        // The worktree is as it was; the copy is the worktree again.
        assert_eq!(contents(&root), before);
        let copy = root.join("target/scratch-test/tree");
        assert_eq!(contents(&copy), before);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_crate_without_integration_tests_or_a_clashing_name_is_refused() {
        let root = tree("refused");
        let outside = root.with_extension("files");
        fs::create_dir_all(&outside).unwrap();
        let file = outside.join("one.rs");
        fs::write(&file, "#[test]\nfn x() {}\n").unwrap();
        let request = |package: &str| Request {
            package: package.to_owned(),
            file: file.clone(),
            rest: Vec::new(),
        };
        let never = |_: &Path, _: &str, _: &[String]| -> Result<(), String> {
            panic!("nothing should run")
        };
        let error = scratch_test(&root, &request("other"), never).unwrap_err();
        assert!(error.contains("has no integration tests"), "{error}");
        // A file of the crate's own is never overwritten, even in the copy.
        let clash = root.join("crates/demo/tests/it/scratch_one.rs");
        fs::write(&clash, "// the crate's own\n").unwrap();
        let error = scratch_test(&root, &request("demo"), never).unwrap_err();
        assert!(error.contains("a file of the crate's own"), "{error}");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_request_and_a_module_name_are_read() {
        let words = |list: &[&str]| {
            list.iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        let request =
            Request::parse(&words(&["cortex-core", "f.rs", "--", "--no-capture"])).unwrap();
        assert_eq!(request.package, "cortex-core");
        assert_eq!(request.rest, ["--no-capture"]);
        assert!(Request::parse(&words(&["cortex-core"])).is_err());
        assert!(Request::parse(&words(&["-p", "x", "f.rs"])).is_err());
        assert_eq!(
            module_name(Path::new("/tmp/Review R4-check.rs")).unwrap(),
            "scratch_review_r4_check"
        );
    }
}
