//! The run itself: the copy of the tree, the results that are kept, the
//! order of the work and the report.
//!
//! What builds and runs the tests is behind [`Runner`], so that everything
//! here is tested without cargo: with a runner that reads the copy and says
//! which tests "fail" for what it finds there.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::{Value, json};

use super::list::{self, Break, Expect};
use super::{apply, jobs, read_source};
use crate::Result;
use crate::junit::{self, TestCase};
use crate::workspace;

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finished {
    /// Every chosen break has a result. `flagged` of them did not do what
    /// their list expects.
    Complete { flagged: usize },
    /// The time budget ran out. `left` pieces of work remain: run again.
    Unfinished { left: usize },
}

/// What a run was asked to do differently.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Options {
    /// Forget the chosen breaks' results first.
    pub(crate) again: bool,
    /// Compile every break before the first is run.
    pub(crate) compile_check: bool,
}

/// What a test run of the copy came to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tested {
    /// The tests were built and run. Every test that ran, passed or not.
    Ran(Vec<TestCase>),
    /// The copy does not compile.
    DoesNotCompile(String),
    /// The run failed some other way: no test is known to have failed, and
    /// none is known to have passed.
    Broken(String),
}

/// What builds the copy and runs its tests. One for each job, each with a
/// copy and a build directory of its own: so it is sent to the job's
/// thread.
pub(crate) trait Runner: Send {
    /// Whether the packages and their tests compile in `tree`. The error is
    /// the compiler's.
    fn compiles(
        &mut self,
        tree: &Path,
        packages: &[String],
    ) -> Result<std::result::Result<(), String>>;

    /// Runs the packages' tests in `tree`.
    fn test(&mut self, tree: &Path, packages: &[String]) -> Result<Tested>;
}

// ------------------------------------------------------------------ cargo

/// The real runner: cargo and nextest, in the copy, with a build directory
/// that is the copy's own.
pub(crate) struct CargoRunner {
    target: PathBuf,
    /// What cargo last printed. Every call writes it anew.
    log: PathBuf,
    /// Where the output of a call that failed is kept, one file a failure.
    logs: PathBuf,
    kept: u32,
}

/// The settings of a test run that a caller may have in its environment. A
/// break's result is kept by the code and the break alone, so its tests are
/// run without them: under seed 0, with the ordinary number of property
/// cases, and writing no snapshot or generated file into the copy.
const CALLERS_SETTINGS: [&str; 6] = [
    "CORTEX_TEST_SEED",
    "CORTEX_TEST_FIRST_SEED",
    "CORTEX_PROPERTY_SEED",
    "CORTEX_PROPERTY_SCALE",
    "CORTEX_UPDATE_GENERATED",
    "CORTEX_UPDATE_SNAPSHOTS",
];

/// nextest's exit status when tests failed, and when the build did.
const NEXTEST_TESTS_FAILED: i32 = 100;
const NEXTEST_BUILD_FAILED: i32 = 101;

impl CargoRunner {
    /// The runner of job `job`, counted from 0: its build directory and its
    /// log are its own. Job 0's are those a run without `--jobs` uses.
    pub(crate) fn new(scratch: &Path, job: usize) -> Self {
        Self {
            target: scratch.join(jobs::numbered("target", job)),
            log: scratch.join(format!("{}.log", jobs::numbered("last", job))),
            logs: scratch.join("logs"),
            kept: 0,
        }
    }

    /// Keeps what cargo last printed under a name of its own, and says
    /// where: the next call writes over the log, and a message that points
    /// at it would then point at another break's output.
    fn keep_log(&mut self) -> String {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        self.kept += 1;
        let job = self
            .log
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("last");
        let kept = self.logs.join(format!("{stamp}-{job}-{}.log", self.kept));
        let copied = fs::create_dir_all(&self.logs).and_then(|()| fs::copy(&self.log, &kept));
        match copied {
            Ok(_) => format!("see {}", kept.display()),
            Err(_) => "its output could not be kept".to_owned(),
        }
    }

    /// cargo, as it is started in the copy.
    fn command(&self, tree: &Path, arguments: &[String]) -> Command {
        let mut command = Command::new(workspace::cargo());
        for setting in CALLERS_SETTINGS {
            command.env_remove(setting);
        }
        command
            .args(arguments)
            .current_dir(tree)
            // The copy never shares a build directory with the tree it was
            // copied from, whatever the caller's environment says.
            .env("CARGO_TARGET_DIR", &self.target)
            .env_remove("CARGO_BUILD_TARGET_DIR")
            // The log is read for the compiler's errors and by a person, not
            // shown on a terminal. CI sets `CARGO_TERM_COLOR=always`, which
            // cargo, rustc and nextest honour even into a file, and an
            // `error` wrapped in escape sequences is not found.
            .env("CARGO_TERM_COLOR", "never");
        command
    }

    /// Runs cargo in `tree` with its output in the log, and returns its
    /// exit status and what it printed.
    fn cargo(&self, tree: &Path, arguments: &[String]) -> Result<(Option<i32>, String)> {
        let shown = arguments.join(" ");
        let log = File::create(&self.log)
            .map_err(|error| format!("could not write {}: {error}", self.log.display()))?;
        let errors = log
            .try_clone()
            .map_err(|error| format!("could not write {}: {error}", self.log.display()))?;
        let status = self
            .command(tree, arguments)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(errors)
            .status()
            .map_err(|error| format!("could not run cargo {shown}: {error}"))?;
        let output = fs::read(&self.log).unwrap_or_default();
        Ok((status.code(), String::from_utf8_lossy(&output).into_owned()))
    }

    fn package_arguments(packages: &[String]) -> Vec<String> {
        packages
            .iter()
            .flat_map(|package| ["-p".to_owned(), package.clone()])
            .collect()
    }
}

/// The compiler's errors in cargo's output, or failing that its last lines
/// that are not cargo's progress (`Checking …`, `Finished …`).
///
/// Escape sequences are taken out first: the runner asks for no colour,
/// and this holds if something asks for it all the same.
fn errors_of(output: &str) -> String {
    let plain = without_escapes(output);
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let errors: Vec<&str> = lines
        .iter()
        .copied()
        // `error[E0425]: ...`, or with `--message-format short`,
        // `src/lib.rs:16:20: error: ...`. Not cargo's own closing line.
        .filter(|line| {
            (line.starts_with("error") || line.contains(": error: ") || line.contains(": error["))
                && !line.starts_with("error: could not compile")
        })
        .take(3)
        .collect();
    if !errors.is_empty() {
        return errors.join(" | ");
    }
    let said: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !is_progress(line))
        .collect();
    said[said.len().saturating_sub(5)..].join(" | ")
}

/// `text` without its ANSI escape sequences: `ESC [` up to the letter that
/// ends a control sequence, or `ESC` and one character.
fn without_escapes(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            plain.push(character);
            continue;
        }
        if characters.next() == Some('[') {
            for inside in characters.by_ref() {
                if ('@'..='~').contains(&inside) {
                    break;
                }
            }
        }
    }
    plain
}

/// Whether a trimmed line is one of cargo's or nextest's progress lines:
/// a status word, then what it is about.
fn is_progress(line: &str) -> bool {
    const STATUS: [&str; 16] = [
        "Adding",
        "Blocking",
        "Building",
        "Checking",
        "Compiling",
        "Downloaded",
        "Downloading",
        "Finished",
        "Fresh",
        "Locking",
        "Running",
        "Starting",
        "Summary",
        "Updating",
        "Waiting",
        "Executable",
    ];
    line.split_once(' ')
        .is_some_and(|(word, _)| STATUS.contains(&word))
}

impl Runner for CargoRunner {
    fn compiles(
        &mut self,
        tree: &Path,
        packages: &[String],
    ) -> Result<std::result::Result<(), String>> {
        let mut arguments: Vec<String> =
            ["check", "--locked", "--tests", "--message-format", "short"]
                .map(str::to_owned)
                .to_vec();
        arguments.extend(Self::package_arguments(packages));
        let (status, output) = self.cargo(tree, &arguments)?;
        Ok(if status == Some(0) {
            Ok(())
        } else {
            Err(errors_of(&output))
        })
    }

    fn test(&mut self, tree: &Path, packages: &[String]) -> Result<Tested> {
        let report = workspace::nextest_report(tree, "mutate");
        // A report left by the run before must not be read as this run's.
        workspace::forget_report(&report)?;
        let mut arguments: Vec<String> = ["nextest", "run", "--locked", "--profile", "mutate"]
            .map(str::to_owned)
            .to_vec();
        arguments.extend(Self::package_arguments(packages));
        let (status, output) = self.cargo(tree, &arguments)?;
        let read = (status == Some(0) || status == Some(NEXTEST_TESTS_FAILED))
            .then(|| junit::read(&report));
        let agree =
            |cases: &[TestCase]| (status == Some(0)) != cases.iter().any(|case| case.failed);
        if let Some(Ok(cases)) = &read
            && agree(cases)
        {
            return Ok(Tested::Ran(cases.clone()));
        }
        if status == Some(NEXTEST_BUILD_FAILED) {
            return Ok(Tested::DoesNotCompile(errors_of(&output)));
        }
        let see_log = self.keep_log();
        Ok(match (status, read) {
            (_, Some(Err(error))) => Tested::Broken(format!("{error}; {see_log}")),
            (_, Some(Ok(_))) => Tested::Broken(format!(
                "nextest's exit status and its report disagree about whether a test failed; \
                 {see_log}"
            )),
            (other, None) => Tested::Broken(format!(
                "nextest ended with {}: {}; {see_log}",
                other.map_or_else(|| "a signal".to_owned(), |code| format!("status {code}")),
                errors_of(&output)
            )),
        })
    }
}

// ------------------------------------------------------------------- copy

/// The files of the tree at `root`, relative to it, with `/`: what git
/// tracks and what it does not ignore; or, where there is no repository (a
/// copy made for a remote build), every file outside `.git` and any
/// `target` directory.
fn tree_files(root: &Path) -> Result<Vec<String>> {
    if root.join(".git").exists() {
        return workspace::repository_files(root);
    }
    let mut files = Vec::new();
    walk(root, "", &mut files, true)?;
    files.sort();
    Ok(files)
}

fn walk(root: &Path, relative: &str, files: &mut Vec<String>, skip_builds: bool) -> Result<()> {
    let directory = root.join(relative);
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("could not list {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not list {}: {error}", directory.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not read {path}: {error}"))?;
        if file_type.is_dir() {
            if skip_builds && (name == "target" || (relative.is_empty() && name == ".git")) {
                continue;
            }
            walk(root, &path, files, skip_builds)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// 64-bit FNV-1a: small, and the same on every machine and toolchain, which
/// is what a key that outlives a run needs.
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn add(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A mark between two pieces, so that "ab", "c" is not "a", "bc".
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// A key for something kept between runs: the code it was found for, what
/// kind of thing it is, and what it is about.
fn key(code: u64, kind: &str, about: &str) -> String {
    let mut hash = Fnv::new();
    hash.add(&code.to_le_bytes());
    hash.add(kind.as_bytes());
    hash.add(about.as_bytes());
    format!("{kind}-{:016x}", hash.0)
}

/// What the code of the tree at `root` is, as one number: every file but
/// the break lists, by name and content. A result is kept for the code it
/// was found with, and a change to a break list must not throw away the
/// results of the breaks it did not change.
fn code_hash(root: &Path, files: &[String]) -> Result<u64> {
    let mut hash = Fnv::new();
    for file in files {
        if file.starts_with(&format!("{}/", list::DIRECTORY)) {
            continue;
        }
        let path = root.join(file);
        let content = fs::read(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        hash.add(file.as_bytes());
        hash.add(&content);
    }
    Ok(hash.0)
}

/// Makes `copy` the same as the tree at `root`: writes the files that
/// differ, and removes what the tree no longer has. A file that is already
/// the same is not touched, so its build stays valid. A break left in the
/// copy by a run that was killed is undone here.
fn sync(root: &Path, copy: &Path, files: &[String]) -> Result<()> {
    for file in files {
        let from = root.join(file);
        let to = copy.join(file);
        let content = fs::read(&from)
            .map_err(|error| format!("could not read {}: {error}", from.display()))?;
        if fs::read(&to).is_ok_and(|existing| existing == content) {
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        // A copy, so that a script stays executable.
        fs::copy(&from, &to)
            .map_err(|error| format!("could not write {}: {error}", to.display()))?;
    }
    let wanted: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    if copy.is_dir() {
        prune(copy, "", &wanted)?;
    }
    Ok(())
}

/// Removes the files under `copy` that are not wanted, and the directories
/// that are then empty: an empty `crates/<name>` is a workspace member with
/// no manifest, to cargo. Returns whether the directory is empty now.
fn prune(copy: &Path, relative: &str, wanted: &BTreeSet<&str>) -> Result<bool> {
    let directory = copy.join(relative);
    let failed = |error: std::io::Error| format!("could not tidy {}: {error}", directory.display());
    let mut empty = true;
    for entry in fs::read_dir(&directory).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        if entry.file_type().map_err(failed)?.is_dir() {
            // nextest keeps its report in the copy's own `target`. The
            // build is elsewhere.
            if path == "target" {
                empty = false;
                continue;
            }
            if prune(copy, &path, wanted)? {
                fs::remove_dir(copy.join(&path)).map_err(failed)?;
            } else {
                empty = false;
            }
        } else if wanted.contains(path.as_str()) {
            empty = false;
        } else {
            fs::remove_file(copy.join(&path)).map_err(failed)?;
        }
    }
    Ok(empty)
}

/// A file of the copy with a break made in it. The file is put back when
/// this is dropped, whatever happened in between.
///
/// Putting it back is a write, so the file is newer than anything built
/// from the broken one and cargo builds it again. That relies on a file
/// system whose times are finer than a build is quick, which ext4, NTFS and
/// APFS all are.
struct Broken {
    path: PathBuf,
    original: String,
    restored: bool,
}

impl Broken {
    fn make(copy: &Path, one: &Break) -> Result<Self> {
        let original = read_source(copy, one)?;
        // The copy holds what the tree held when the breaks were checked,
        // so this cannot fail unless the tree changed under the run. If it
        // does, the run stops: a break is never run without being made.
        let broken = apply(one, &original)?;
        let path = copy.join(&one.file);
        fs::write(&path, broken)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        Ok(Self {
            path,
            original,
            restored: false,
        })
    }

    fn restore(mut self) -> Result<()> {
        self.restored = true;
        fs::write(&self.path, &self.original)
            .map_err(|error| format!("could not put {} back: {error}", self.path.display()))
    }
}

impl Drop for Broken {
    fn drop(&mut self) {
        if !self.restored {
            // Reached on an error or a panic. The next run's sync puts the
            // file back too, so a failure here loses nothing.
            let _ = fs::write(&self.path, &self.original);
        }
    }
}

// ---------------------------------------------------------------- results

/// What a break's test run came to.
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    /// These tests failed (`<binary> <name>`). None: the break survived.
    Ran {
        failed: Vec<String>,
        seconds: f64,
    },
    DoesNotCompile(String),
    Broken(String),
}

/// Everything known so far, by key. It is a file of lines, one for each
/// thing learnt, added to as the run goes: a run that is killed loses at
/// most the piece of work it was in.
struct Results {
    path: PathBuf,
    lines: BTreeMap<String, Value>,
}

impl Results {
    fn load(path: &Path) -> Self {
        let mut lines = BTreeMap::new();
        for line in fs::read_to_string(path).unwrap_or_default().lines() {
            // A line cut short by a kill is not JSON, and is skipped.
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && let Some(key) = value.get("key").and_then(Value::as_str)
            {
                if value.get("forgotten").is_some() {
                    lines.remove(key);
                } else {
                    lines.insert(key.to_owned(), value.clone());
                }
            }
        }
        Self {
            path: path.to_owned(),
            lines,
        }
    }

    fn put(&mut self, key: &str, mut value: Value) -> Result<()> {
        value["key"] = json!(key);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| format!("could not write {}: {error}", self.path.display()))?;
        // A line of its own even after a line that was cut short.
        writeln!(file, "\n{value}")
            .map_err(|error| format!("could not write {}: {error}", self.path.display()))?;
        if value.get("forgotten").is_some() {
            self.lines.remove(key);
        } else {
            self.lines.insert(key.to_owned(), value);
        }
        Ok(())
    }

    fn forget(&mut self, key: &str) -> Result<()> {
        if self.lines.contains_key(key) {
            self.put(key, json!({ "forgotten": true }))?;
        }
        Ok(())
    }

    /// The tests that ran, and passed, with nothing broken.
    fn baseline(&self, key: &str) -> Option<Vec<String>> {
        let tests = self.lines.get(key)?.get("tests")?.as_array()?;
        Some(
            tests
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        )
    }

    /// Whether a break compiles: `Some(None)` when it does, `Some(Some(_))`
    /// with the error when it does not, `None` when it is not known.
    fn compiled(&self, key: &str) -> Option<Option<String>> {
        let line = self.lines.get(key)?;
        Some(line.get("error").and_then(Value::as_str).map(str::to_owned))
    }

    fn outcome(&self, key: &str) -> Option<Outcome> {
        let line = self.lines.get(key)?;
        let detail = || {
            line.get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match line.get("status")?.as_str()? {
            "ran" => Some(Outcome::Ran {
                failed: line
                    .get("failed")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                seconds: line.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
            }),
            "does-not-compile" => Some(Outcome::DoesNotCompile(detail())),
            "broken" => Some(Outcome::Broken(detail())),
            _ => None,
        }
    }

    fn put_outcome(&mut self, key: &str, id: &str, outcome: &Outcome) -> Result<()> {
        let value = match outcome {
            Outcome::Ran { failed, seconds } => {
                json!({ "break": id, "status": "ran", "failed": failed, "seconds": seconds })
            }
            Outcome::DoesNotCompile(detail) => {
                json!({ "break": id, "status": "does-not-compile", "detail": detail })
            }
            Outcome::Broken(detail) => {
                json!({ "break": id, "status": "broken", "detail": detail })
            }
        };
        self.put(key, value)
    }
}

/// What is to be said of a break whose result is known.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// It did what its list expects. The words say what that was.
    Good(String),
    /// It did not: a label for the list of flagged breaks, and what to do.
    Flagged(&'static str, String),
}

/// Judges a break by what happened when it was made. `tests` is every test
/// that ran with nothing broken.
fn judge(one: &Break, outcome: &Outcome, tests: &[String]) -> Verdict {
    let named = |entry: &str, id: &str| {
        id.split_once(' ')
            .is_some_and(|(binary, name)| list::names_test(entry, binary, name))
    };
    match outcome {
        Outcome::DoesNotCompile(detail) => Verdict::Flagged(
            "DOES NOT COMPILE",
            format!(
                "With this break made the code does not build, so no test was run: {detail}\n\
                 Correct its `replace`."
            ),
        ),
        Outcome::Broken(detail) => Verdict::Flagged(
            "COULD NOT BE JUDGED",
            format!("The test run failed without naming a failed test: {detail}"),
        ),
        Outcome::Ran { failed, .. } if one.expect == Expect::Survives => {
            if failed.is_empty() {
                Verdict::Good("survived, as its list expects: no test failed".to_owned())
            } else {
                Verdict::Flagged(
                    "CAUGHT, AND EXPECTED TO SURVIVE",
                    format!(
                        "Its list says no test can catch it, and these did: {}.\nTake \
                         `expect: survives` off it, and name the tests in `caught-by`.",
                        some_of(failed)
                    ),
                )
            }
        }
        Outcome::Ran { failed, .. } if failed.is_empty() && !one.caught_by.is_empty() => {
            Verdict::Flagged(
                "NOT CAUGHT",
                format!(
                    "No test failed with this break made, though its list names {}. If they \
                     still test this rule, another guard in the code may now stop what the \
                     break lets through (step 13a closed a file by name, then its whole \
                     folder, and the break of the first could no longer fail). Break both \
                     guards in one break, or mark it `expect: survives` and name the other \
                     guard in its `rule`. Otherwise write a test that fails with it.",
                    quoted(&one.caught_by.iter().map(String::as_str).collect::<Vec<_>>())
                ),
            )
        }
        Outcome::Ran { failed, .. } if failed.is_empty() => Verdict::Flagged(
            "NOT CAUGHT",
            "No test failed with this break made. Write a test that fails with it. If nothing \
             a test can see is changed by it, mark it `expect: survives` and say why in its \
             `rule`."
                .to_owned(),
        ),
        Outcome::Ran { failed, .. } => {
            // A name that no test has is a test that was renamed or
            // removed, and would otherwise look like a test that passed.
            let unknown: Vec<&str> = one
                .caught_by
                .iter()
                .filter(|entry| !tests.iter().any(|id| named(entry, id)))
                .map(String::as_str)
                .collect();
            if !unknown.is_empty() {
                return Verdict::Flagged(
                    "NAMES A TEST THAT DOES NOT EXIST",
                    format!(
                        "No test of its packages is called {}. It was caught by: {}.\nCorrect \
                         its `caught-by`.",
                        quoted(&unknown),
                        some_of(failed)
                    ),
                );
            }
            let missed: Vec<&str> = one
                .caught_by
                .iter()
                .filter(|entry| !failed.iter().any(|id| named(entry, id)))
                .map(String::as_str)
                .collect();
            if !missed.is_empty() {
                return Verdict::Flagged(
                    "NOT CAUGHT BY ITS OWN TESTS",
                    format!(
                        "Its list says {} must fail, and with the break made it passed. It was \
                         caught by: {}.\nA test that used to catch this no longer does.",
                        quoted(&missed),
                        some_of(failed)
                    ),
                );
            }
            Verdict::Good(format!(
                "caught by {} test(s): {}",
                failed.len(),
                some_of(failed)
            ))
        }
    }
}

fn quoted(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first few of a list of tests, and how many more there are.
fn some_of(tests: &[String]) -> String {
    const SHOWN: usize = 4;
    let mut text = tests
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if tests.len() > SHOWN {
        text.push_str(&format!(", and {} more", tests.len() - SHOWN));
    }
    text
}

// -------------------------------------------------------------------- run

/// Takes the copy for this run, for as long as the file returned is held.
///
/// Two runs on one copy would undo each other's breaks: the second brings
/// the copy up to date while the first is running a break's tests, and the
/// first then reports a break as survived that was no longer made. The lock
/// is the operating system's, so a run that is killed lets go of it.
fn lock(scratch: &Path) -> Result<File> {
    let path = scratch.join("lock");
    let file = OpenOptions::new()
        // For reading and writing: Windows locks no file that is open only
        // for appending.
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(format!(
            "another `cargo xtask mutate` is running on the copy in {}. Two runs on one copy \
             would undo each other's breaks. Wait for it to end and run this again.",
            scratch.display()
        )),
        // Not taken, and not because another run has it. Going on would be
        // going on without knowing whether another run is there.
        Err(fs::TryLockError::Error(error)) => Err(format!(
            "could not lock {}: {error}. One run at a time may use a copy, and without the \
             lock there is no telling whether another is using it.",
            path.display()
        )),
    }
}

/// The keys of what is kept for one break.
struct Keys {
    compile: String,
    run: String,
    /// The tests of the break's packages with nothing broken.
    baseline: String,
    /// The packages the break's tests are run for.
    packages: Vec<String>,
}

impl Keys {
    fn of(code: u64, one: &Break) -> Result<Self> {
        let packages = one.test_packages()?;
        // The same packages in another order are the same build.
        let mut set = packages.clone();
        set.sort();
        set.dedup();
        Ok(Self {
            compile: key(code, "compile", &one.definition()),
            run: key(code, "run", &one.definition()),
            baseline: key(code, "baseline", &set.join(",")),
            packages,
        })
    }

    fn all(code: u64, breaks: &[Break]) -> Result<Vec<Self>> {
        breaks.iter().map(|one| Self::of(code, one)).collect()
    }
}

/// The work of a run, for counting what is left of it.
struct Plan<'a> {
    keys: &'a [Keys],
    compile_check: bool,
}

impl Plan<'_> {
    /// The pieces of work not done yet: the tests with nothing broken, once
    /// for each set of packages a break runs, a compile for each break, and
    /// a test run for each break that compiles.
    fn left(&self, results: &Results) -> usize {
        let to_compile = self
            .keys
            .iter()
            .filter(|keys| self.compile_check && results.compiled(&keys.compile).is_none())
            .count();
        let to_run = self
            .keys
            .iter()
            .filter(|keys| {
                results.outcome(&keys.run).is_none()
                    && !matches!(results.compiled(&keys.compile), Some(Some(_)))
            })
            .count();
        let to_try = self
            .keys
            .iter()
            .map(|keys| &keys.baseline)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|key| results.baseline(key).is_none())
            .count();
        to_try + to_compile + to_run
    }

    /// Says that the time ran out, and how much is left.
    fn stop(&self, results: &Results, say: &mut dyn FnMut(&str)) -> Finished {
        let left = self.left(results);
        say(&format!(
            "\nOut of time with {left} piece(s) of work left. Everything so far is kept."
        ));
        Finished::Unfinished { left }
    }
}

/// Runs the breaks: see the module's description for the order of the
/// work. `out_of_time` is asked before each piece of work is started.
#[cfg(test)]
pub(crate) fn drive(
    root: &Path,
    scratch: &Path,
    breaks: &[Break],
    options: &Options,
    runner: &mut dyn Runner,
    out_of_time: &mut dyn FnMut() -> bool,
    say: &mut dyn FnMut(&str),
) -> Result<Finished> {
    drive_jobs(
        root,
        scratch,
        breaks,
        options,
        &mut [runner],
        out_of_time,
        say,
    )
}

/// [`drive`], with the work shared out among `runners`, one job each, in a
/// copy of the tree of its own (`--jobs`). The pieces of work are the same
/// and come in the same three rounds, each finished before the next
/// begins: the tests with nothing broken, every break compiled, every
/// break's tests. With more than one job `out_of_time` is asked as each
/// piece finishes and a few times a second, and no piece is started after
/// it says yes.
pub(crate) fn drive_jobs(
    root: &Path,
    scratch: &Path,
    breaks: &[Break],
    options: &Options,
    runners: &mut [&mut dyn Runner],
    out_of_time: &mut dyn FnMut() -> bool,
    say: &mut dyn FnMut(&str),
) -> Result<Finished> {
    fs::create_dir_all(scratch)
        .map_err(|error| format!("could not create {}: {error}", scratch.display()))?;
    let _lock = lock(scratch)?;
    let files = tree_files(root)?;
    let code = code_hash(root, &files)?;
    let copies: Vec<PathBuf> = (0..runners.len())
        .map(|job| jobs::copy(scratch, job))
        .collect();
    for copy in &copies {
        sync(root, copy, &files)?;
    }
    let mut results = Results::load(&scratch.join("results.jsonl"));
    let keys = Keys::all(code, breaks)?;
    for keys in &keys {
        // What failed without a verdict is tried again by every run: the
        // cause may have been the machine's (a full disk, a killed
        // process), and a result that is kept for good would then be wrong
        // for good. A break that really does not compile costs a second.
        let no_verdict = matches!(
            results.outcome(&keys.run),
            Some(Outcome::Broken(_) | Outcome::DoesNotCompile(_))
        );
        if options.again || no_verdict {
            results.forget(&keys.run)?;
        }
        if options.again || matches!(results.compiled(&keys.compile), Some(Some(_))) {
            results.forget(&keys.compile)?;
        }
    }

    let plan = Plan {
        keys: &keys,
        compile_check: options.compile_check,
    };

    // The tests, with nothing broken: once for each set of packages that a
    // break runs. Which tests there are depends on what is built together
    // (a feature one crate's tests turn on in another), so a break is
    // judged against the tests of exactly the build it runs in. And what
    // is learnt holds whichever breaks are chosen next time.
    let mut sets: Vec<(&[String], &str)> = Vec::new();
    for one in &keys {
        let wanted = results.baseline(&one.baseline).is_none()
            && !sets.iter().any(|(_, key)| *key == one.baseline);
        if wanted {
            sets.push((&one.packages, &one.baseline));
        }
    }
    let finished = jobs::for_each(
        runners,
        &copies,
        &sets,
        &|runner, copy, (packages, _)| runner.test(copy, packages),
        out_of_time,
        &mut |(packages, baseline_key), tested| {
            let package = packages.join(", ");
            say(&format!("The tests of {package} with nothing broken ..."));
            let cases = match tested? {
                Tested::Ran(cases) => cases,
                Tested::DoesNotCompile(detail) => {
                    return Err(format!(
                        "the tree does not compile with nothing broken: {detail}"
                    ));
                }
                Tested::Broken(detail) => {
                    return Err(format!(
                        "the tests of {package} could not be run with nothing broken: {detail}"
                    ));
                }
            };
            let failing: Vec<String> = cases
                .iter()
                .filter(|case| case.failed)
                .map(TestCase::id)
                .collect();
            if !failing.is_empty() {
                return Err(format!(
                    "{} test(s) fail with nothing broken: {}.\nA test that fails by itself is \
                     no evidence about a break. Fix it first; if it fails only sometimes, find \
                     it with `cargo xtask stress`.",
                    failing.len(),
                    some_of(&failing)
                ));
            }
            let tests: Vec<String> = cases.iter().map(TestCase::id).collect();
            say(&format!("  {} tests pass.", tests.len()));
            results.put(baseline_key, json!({ "tests": tests }))
        },
    )?;
    if !finished {
        return Ok(plan.stop(&results, say));
    }

    // Every break compiles, before any is run.
    if options.compile_check {
        let unknown: Vec<(&Break, &Keys)> = breaks
            .iter()
            .zip(&keys)
            .filter(|(_, keys)| results.compiled(&keys.compile).is_none())
            .collect();
        if !unknown.is_empty() {
            say(&format!("Compiling {} break(s) ...", unknown.len()));
        }
        let finished = jobs::for_each(
            runners,
            &copies,
            &unknown,
            &|runner, copy, (one, keys)| {
                let broken = Broken::make(copy, one)?;
                let compiled = runner.compiles(copy, &keys.packages);
                broken.restore()?;
                compiled
            },
            out_of_time,
            &mut |(one, keys), compiled| match compiled? {
                Ok(()) => results.put(&keys.compile, json!({ "break": one.id })),
                Err(error) => {
                    say(&format!("  does not compile: {}: {error}", one.id));
                    results.put(&keys.compile, json!({ "break": one.id, "error": error }))
                }
            },
        )?;
        if !finished {
            return Ok(plan.stop(&results, say));
        }
    }

    // Each break's tests.
    let total = breaks.len();
    let width = total.to_string().len();
    let to_run: Vec<(usize, &Break, &Keys)> = breaks
        .iter()
        .zip(&keys)
        .enumerate()
        .filter(|(_, (_, keys))| {
            results.outcome(&keys.run).is_none()
                && !matches!(results.compiled(&keys.compile), Some(Some(_)))
        })
        .map(|(index, (one, keys))| (index, one, keys))
        .collect();
    let finished = jobs::for_each(
        runners,
        &copies,
        &to_run,
        &|runner, copy, (_, one, keys)| -> Result<(Tested, f64)> {
            let started = Instant::now();
            let broken = Broken::make(copy, one)?;
            let tested = runner.test(copy, &keys.packages);
            broken.restore()?;
            Ok((tested?, started.elapsed().as_secs_f64()))
        },
        out_of_time,
        &mut |(index, one, keys), tested| {
            let (tested, seconds) = tested?;
            let outcome = match tested {
                Tested::Ran(cases) => {
                    // In name order: a report lists tests as they
                    // finished, which is not the same from one run to the
                    // next.
                    let mut failed: Vec<String> = cases
                        .iter()
                        .filter(|case| case.failed)
                        .map(TestCase::id)
                        .collect();
                    failed.sort();
                    Outcome::Ran { failed, seconds }
                }
                Tested::DoesNotCompile(detail) => Outcome::DoesNotCompile(detail),
                Tested::Broken(detail) => Outcome::Broken(detail),
            };
            results.put_outcome(&keys.run, &one.id, &outcome)?;
            let tests = results.baseline(&keys.baseline).unwrap_or_default();
            let line = match judge(one, &outcome, &tests) {
                Verdict::Good(words) => words,
                Verdict::Flagged(label, _) => label.to_owned(),
            };
            say(&format!(
                "[{:>width$}/{total}] {}  {line}  ({seconds:.1} s)",
                index + 1,
                one.id,
            ));
            Ok(())
        },
    )?;
    if !finished {
        return Ok(plan.stop(&results, say));
    }

    Ok(report(breaks, &keys, &results, say))
}

/// `--report`: what is known for the tree as it is, without running
/// anything.
pub(crate) fn report_only(
    root: &Path,
    scratch: &Path,
    breaks: &[Break],
    say: &mut dyn FnMut(&str),
) -> Result<Finished> {
    let files = tree_files(root)?;
    let code = code_hash(root, &files)?;
    let results = Results::load(&scratch.join("results.jsonl"));
    let keys = Keys::all(code, breaks)?;
    Ok(report(breaks, &keys, &results, say))
}

/// Prints what is known of every break, and the flagged ones with what to
/// do about each.
fn report(
    breaks: &[Break],
    keys: &[Keys],
    results: &Results,
    say: &mut dyn FnMut(&str),
) -> Finished {
    let mut good = 0;
    let mut unknown = 0;
    let mut flagged: Vec<(&Break, &'static str, String)> = Vec::new();
    for (one, keys) in breaks.iter().zip(keys) {
        let outcome = match results.compiled(&keys.compile) {
            Some(Some(error)) => Some(Outcome::DoesNotCompile(error)),
            _ => results.outcome(&keys.run),
        };
        let tests = results.baseline(&keys.baseline).unwrap_or_default();
        match outcome.map(|outcome| judge(one, &outcome, &tests)) {
            None => unknown += 1,
            Some(Verdict::Good(_)) => good += 1,
            Some(Verdict::Flagged(label, what)) => flagged.push((one, label, what)),
        }
    }
    say(&format!(
        "\n{} break(s): {good} did what the list expects, {} flagged{}.",
        breaks.len(),
        flagged.len(),
        if unknown > 0 {
            format!(", {unknown} not run yet")
        } else {
            String::new()
        }
    ));
    for (one, label, what) in &flagged {
        say(&format!(
            "\n{label}: {}  ({})\n  rule: {}\n  {}",
            one.id,
            one.place(),
            one.rule,
            what.replace('\n', "\n  ")
        ));
    }
    if unknown > 0 {
        Finished::Unfinished { left: unknown }
    } else {
        Finished::Complete {
            flagged: flagged.len(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::super::list::{self, Break};
    use super::super::tests::breaks;
    use super::{
        CargoRunner, Finished, Options, Outcome, Runner, Tested, Verdict, drive, report_only,
    };
    use crate::Result;
    use crate::junit::TestCase;

    /// A tree of the test's own, made new, outside any repository.
    pub(crate) fn tree(test: &str, files: &[(&str, &str)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("cortex-xtask-mutate-{test}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        for (path, content) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        root
    }

    const LIB: &str = "\
pub fn over(seconds: f64, budget: f64) -> bool {
    seconds > budget
}

pub fn far_over(seconds: f64, budget: f64) -> bool {
    seconds > 1.5 * budget
}

pub fn label(over: bool) -> &'static str {
    if over { \"over\" } else { \"within\" }
}
";

    const LIST: &str = "\
[break] over-is-strict
rule: A tier at its budget is not over it.
file: crates/demo/src/lib.rs
in: over
find: seconds > budget
replace: seconds >= budget
caught-by:
    tests::at_the_budget_is_not_over

[break] far-over-factor
rule: Far over is one and a half budgets.
file: crates/demo/src/lib.rs
find: 1.5 * budget
replace: 1.0 * budget
caught-by:
    demo tests::the_line_is_one_and_a_half
    tests::far_*

[break] label-unseen
rule: The label of a tier inside its budget. Nothing tests it.
file: crates/demo/src/lib.rs
find: \"within\"
replace: \"inside\"
";

    /// A runner that reads the copy and decides, from what the library
    /// holds, which of four tests fail: what the real tests would do.
    #[derive(Default)]
    struct Pretend {
        /// What was asked of it, in order: `compile` or `test`, and the
        /// packages.
        asked: Vec<String>,
        /// A test that fails whatever the code is.
        always_failing: Option<&'static str>,
        /// How many test runs of a broken tree fail without a verdict, as
        /// a run does when the machine gets in its way.
        runs_to_lose: usize,
    }

    impl Pretend {
        fn library(tree: &Path) -> String {
            fs::read_to_string(tree.join("crates/demo/src/lib.rs")).unwrap()
        }
    }

    impl Runner for Pretend {
        fn compiles(
            &mut self,
            tree: &Path,
            packages: &[String],
        ) -> Result<std::result::Result<(), String>> {
            self.asked.push(format!("compile {}", packages.join(",")));
            Ok(if Self::library(tree).contains("does not compile") {
                Err("error[E0425]: cannot find value".to_owned())
            } else {
                Ok(())
            })
        }

        fn test(&mut self, tree: &Path, packages: &[String]) -> Result<Tested> {
            self.asked.push(format!("test {}", packages.join(",")));
            let library = Self::library(tree);
            if library.contains("does not compile") {
                return Ok(Tested::DoesNotCompile("error[E0425]".to_owned()));
            }
            if library.contains("breaks the run") {
                return Ok(Tested::Broken("no report".to_owned()));
            }
            if library != LIB && self.runs_to_lose > 0 {
                self.runs_to_lose -= 1;
                return Ok(Tested::Broken("the disk was full".to_owned()));
            }
            let case = |name: &str, failed: bool| TestCase {
                binary: "demo".to_owned(),
                name: name.to_owned(),
                seconds: 0.01,
                failed: failed || self.always_failing == Some(name),
                retried: false,
                repeat: None,
            };
            Ok(Tested::Ran(vec![
                case(
                    "tests::at_the_budget_is_not_over",
                    !library.contains("seconds > budget"),
                ),
                case(
                    "tests::the_line_is_one_and_a_half",
                    !library.contains("1.5 * budget"),
                ),
                case("tests::far_over_is_over", !library.contains("1.5 * budget")),
                case("tests::unrelated", false),
            ]))
        }
    }

    struct Run {
        root: PathBuf,
        scratch: PathBuf,
        runner: Pretend,
        said: String,
    }

    impl Drop for Run {
        fn drop(&mut self) {
            // What a failed test left is the evidence, and the next run of
            // the test removes it.
            if !std::thread::panicking() {
                let _ = fs::remove_dir_all(&self.root);
                let _ = fs::remove_dir_all(&self.scratch);
            }
        }
    }

    impl Run {
        fn new(test: &str) -> Self {
            let root = tree(
                test,
                &[
                    ("crates/demo/Cargo.toml", "[package]\nname = \"demo\"\n"),
                    ("crates/demo/src/lib.rs", LIB),
                    ("README.md", "A demo.\n"),
                ],
            );
            let scratch = root.with_extension("scratch");
            if scratch.exists() {
                fs::remove_dir_all(&scratch).unwrap();
            }
            fs::create_dir_all(&scratch).unwrap();
            Self {
                scratch,
                root,
                runner: Pretend::default(),
                said: String::new(),
            }
        }

        /// Runs the breaks, with time for `pieces` pieces of work.
        fn go_for(&mut self, list: &[Break], pieces: usize) -> Result<Finished> {
            let mut started = 0;
            let options = Options {
                again: false,
                compile_check: true,
            };
            drive(
                &self.root,
                &self.scratch,
                list,
                &options,
                &mut self.runner,
                &mut || {
                    started += 1;
                    started > pieces
                },
                &mut |line| {
                    self.said.push_str(line);
                    self.said.push('\n');
                },
            )
        }

        fn go(&mut self, list: &[Break]) -> Result<Finished> {
            self.go_for(list, usize::MAX)
        }

        fn copy_of_the_library(&self) -> String {
            fs::read_to_string(self.scratch.join("tree/crates/demo/src/lib.rs")).unwrap()
        }
    }

    #[test]
    fn cargo_is_started_in_the_copy_with_its_own_build_directory_and_none_of_the_callers_test_settings()
     {
        let scratch = Path::new("scratch");
        let runner = CargoRunner::new(scratch, 0);
        let command = runner.command(&scratch.join("tree"), &["check".to_owned()]);
        let variable = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };

        assert_eq!(
            command.get_current_dir(),
            Some(scratch.join("tree").as_path())
        );
        assert_eq!(
            variable("CARGO_TARGET_DIR"),
            Some(Some(scratch.join("target").to_string_lossy().into_owned()))
        );
        for removed in [
            "CARGO_BUILD_TARGET_DIR",
            "CORTEX_TEST_SEED",
            "CORTEX_PROPERTY_SCALE",
            "CORTEX_UPDATE_GENERATED",
            "CORTEX_UPDATE_SNAPSHOTS",
        ] {
            assert_eq!(variable(removed), Some(None), "{removed}");
        }
        // Whatever the caller asked for, a log with no colour in it.
        assert_eq!(variable("CARGO_TERM_COLOR"), Some(Some("never".to_owned())));
    }

    #[test]
    fn each_job_has_a_build_directory_and_a_log_of_its_own() {
        let scratch = Path::new("scratch");
        let first = CargoRunner::new(scratch, 0);
        assert_eq!(first.target, scratch.join("target"));
        assert_eq!(first.log, scratch.join("last.log"));
        let third = CargoRunner::new(scratch, 2);
        assert_eq!(third.target, scratch.join("target-3"));
        assert_eq!(third.log, scratch.join("last-3.log"));
        assert_eq!(super::jobs::copy(scratch, 2), scratch.join("tree-3"));
    }

    #[test]
    fn the_compilers_errors_are_picked_out_of_cargos_output() {
        let short = "    Checking demo v0.0.0 (/somewhere/crates/demo)
crates/demo/src/lib.rs:16:20: error: expected one of `!` or `.`, found `the`
crates/demo/src/lib.rs:20:1: error[E0425]: cannot find value `x` in this scope
error: could not compile `demo` (lib test) due to 2 previous errors
";
        assert_eq!(
            super::errors_of(short),
            "crates/demo/src/lib.rs:16:20: error: expected one of `!` or `.`, found `the` | \
             crates/demo/src/lib.rs:20:1: error[E0425]: cannot find value `x` in this scope"
        );
        assert_eq!(
            super::errors_of("error[E0308]: mismatched types\n  --> src/lib.rs:1:1\n"),
            "error[E0308]: mismatched types"
        );
        // With no error to quote, the end of what cargo said.
        assert_eq!(
            super::errors_of("one\ntwo\nthree\nfour\nfive\nsix\n"),
            "two | three | four | five | six"
        );
    }

    /// The nightly run of 2026-11-14 (issue #40): CI sets
    /// `CARGO_TERM_COLOR=always`, every `error` came wrapped in escape
    /// sequences, none was found, and the message was cargo's last lines,
    /// `Checking demo …` first. This is that run's output, as it was.
    #[test]
    fn coloured_output_still_gives_the_compilers_errors_and_never_cargos_progress() {
        let coloured = "\u{1b}[1m\u{1b}[92m    Checking\u{1b}[0m demo v0.0.0 (/runner/tree/crates/demo)\n\
            crates/demo/src/lib.rs:16:20: \u{1b}[1m\u{1b}[91merror\u{1b}[0m: expected one of `!`, `.`, \
            or an operator, found `the`: expected one of 8 possible tokens\n\
            \u{1b}[1m\u{1b}[91merror\u{1b}[0m: could not compile `demo` (lib test) due to 1 previous error\n";
        assert_eq!(
            super::errors_of(coloured),
            "crates/demo/src/lib.rs:16:20: error: expected one of `!`, `.`, or an operator, \
             found `the`: expected one of 8 possible tokens"
        );
        // No error to quote: the last lines, without cargo's progress.
        let progress = "\u{1b}[1m\u{1b}[92m   Compiling\u{1b}[0m demo v0.0.0 (/tree)\n\
            linker `cc` not found\n\
            \u{1b}[1m\u{1b}[92m    Finished\u{1b}[0m `test` profile [unoptimized] target(s) in 0.1s\n\
            note: something else\n";
        assert_eq!(
            super::errors_of(progress),
            "linker `cc` not found | note: something else"
        );
        assert_eq!(
            super::without_escapes("a\u{1b}[1;31mb\u{1b}[0mc\u{1b}=d"),
            "abcd"
        );
    }

    /// Runs `list` with `jobs` runners that pretend, and time for
    /// `pieces` pieces of work. Returns what each runner was asked.
    fn go_with_jobs(
        run: &mut Run,
        list: &[Break],
        jobs: usize,
        pieces: usize,
    ) -> (Result<Finished>, Vec<Vec<String>>) {
        let mut pretenders: Vec<Pretend> = (0..jobs).map(|_| Pretend::default()).collect();
        let mut runners: Vec<&mut dyn Runner> = pretenders
            .iter_mut()
            .map(|runner| runner as &mut dyn Runner)
            .collect();
        let mut asked_for_time = 0;
        let finished = super::drive_jobs(
            &run.root,
            &run.scratch,
            list,
            &Options {
                again: false,
                compile_check: true,
            },
            &mut runners,
            &mut || {
                asked_for_time += 1;
                asked_for_time > pieces
            },
            &mut |line| {
                run.said.push_str(line);
                run.said.push('\n');
            },
        );
        drop(runners);
        (
            finished,
            pretenders.into_iter().map(|runner| runner.asked).collect(),
        )
    }

    #[test]
    fn three_jobs_judge_the_breaks_as_one_does() {
        let mut run = Run::new("jobs");
        let list = breaks("demo", LIST);

        let (finished, asked) = go_with_jobs(&mut run, &list, 3, usize::MAX);

        assert_eq!(finished.unwrap(), Finished::Complete { flagged: 1 });
        for line in [
            "[1/3] demo/over-is-strict  caught by 1 test(s): demo tests::at_the_budget_is_not_over",
            "[2/3] demo/far-over-factor  caught by 2 test(s): demo tests::far_over_is_over, demo \
             tests::the_line_is_one_and_a_half",
            "[3/3] demo/label-unseen  NOT CAUGHT",
            "3 break(s): 2 did what the list expects, 1 flagged.",
        ] {
            assert!(run.said.contains(line), "{line}\n{}", run.said);
        }
        // Every piece of work was done once, by one job or another: the
        // tests with nothing broken, three compiles and three runs.
        let mut all: Vec<&String> = asked.iter().flatten().collect();
        all.sort();
        assert_eq!(
            all,
            [
                "compile demo",
                "compile demo",
                "compile demo",
                "test demo",
                "test demo",
                "test demo",
                "test demo"
            ]
        );
        // Each job had a copy of its own, and every copy is back as the
        // tree is: no break is left in one.
        for copy in ["tree", "tree-2", "tree-3"] {
            let library = run.scratch.join(copy).join("crates/demo/src/lib.rs");
            assert_eq!(fs::read_to_string(&library).unwrap(), LIB, "{copy}");
        }

        // What three jobs learnt, one job knows: nothing is run again.
        let mut again = Pretend::default();
        let finished = drive(
            &run.root,
            &run.scratch,
            &list,
            &Options {
                again: false,
                compile_check: true,
            },
            &mut again,
            &mut || false,
            &mut |_| {},
        );
        assert_eq!(finished.unwrap(), Finished::Complete { flagged: 1 });
        assert_eq!(again.asked, Vec::<String>::new());
    }

    #[test]
    fn jobs_start_nothing_once_the_time_is_up_and_the_next_run_carries_on() {
        let mut run = Run::new("jobs-time");
        let list = breaks("demo", LIST);

        // Time for nothing at all.
        let (finished, asked) = go_with_jobs(&mut run, &list, 2, 0);
        assert_eq!(finished.unwrap(), Finished::Unfinished { left: 7 });
        assert!(asked.iter().all(Vec::is_empty), "{asked:?}");

        // Then all the time there is: the run finishes the work.
        let (finished, _) = go_with_jobs(&mut run, &list, 2, usize::MAX);
        assert_eq!(finished.unwrap(), Finished::Complete { flagged: 1 });
    }

    #[test]
    fn a_job_that_finds_a_failing_test_with_nothing_broken_stops_the_run() {
        let mut run = Run::new("jobs-failing");
        let list = breaks("demo", LIST);
        let mut first = Pretend {
            always_failing: Some("tests::unrelated"),
            ..Pretend::default()
        };
        let mut second = Pretend {
            always_failing: Some("tests::unrelated"),
            ..Pretend::default()
        };
        let mut runners: Vec<&mut dyn Runner> = vec![&mut first, &mut second];
        let error = super::drive_jobs(
            &run.root,
            &run.scratch,
            &list,
            &Options::default(),
            &mut runners,
            &mut || false,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(error.contains("fail with nothing broken"), "{error}");
        drop(runners);
        let breaks_run = first.asked.len() + second.asked.len();
        assert_eq!(breaks_run, 1, "only the tests with nothing broken ran");
        run.said.clear();
    }

    #[test]
    fn breaks_that_are_caught_pass_and_the_one_no_test_sees_is_flagged() {
        let mut run = Run::new("caught");
        let list = breaks("demo", LIST);

        let finished = run.go(&list).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert!(
            run.said.contains(
                "[1/3] demo/over-is-strict  caught by 1 test(s): demo \
                 tests::at_the_budget_is_not_over"
            ),
            "{}",
            run.said
        );
        assert!(
            run.said.contains(
                "[2/3] demo/far-over-factor  caught by 2 test(s): demo \
                 tests::far_over_is_over, demo tests::the_line_is_one_and_a_half"
            ),
            "{}",
            run.said
        );
        assert!(
            run.said.contains("[3/3] demo/label-unseen  NOT CAUGHT"),
            "{}",
            run.said
        );
        assert!(
            run.said
                .contains("3 break(s): 2 did what the list expects, 1 flagged."),
            "{}",
            run.said
        );
        assert!(
            run.said.contains(
                "NOT CAUGHT: demo/label-unseen  (mutations/demo.breaks:19)\n  rule: The label \
                 of a tier inside its budget."
            ),
            "{}",
            run.said
        );
    }

    /// A break its list says is caught, which nothing catches now, may have
    /// been made unable to fail by a second guard in the code: the message
    /// says so, and what to do.
    #[test]
    fn a_break_with_tests_named_that_nothing_catches_points_at_a_second_guard() {
        let named = list::parse(
            "mutations/demo.breaks",
            "[break] b\nrule: r\nfile: f.rs\nfind: x\nreplace: y\ncaught-by:\n    t::one\n",
        )
        .unwrap()
        .remove(0);
        let unnamed = list::parse(
            "mutations/demo.breaks",
            "[break] b\nrule: r\nfile: f.rs\nfind: x\nreplace: y\n",
        )
        .unwrap()
        .remove(0);
        let nothing = Outcome::Ran {
            failed: Vec::new(),
            seconds: 1.0,
        };
        let tests = ["demo t::one".to_owned()];
        let Verdict::Flagged(label, advice) = super::judge(&named, &nothing, &tests) else {
            panic!("not flagged");
        };
        assert_eq!(label, "NOT CAUGHT");
        assert!(
            advice.contains("though its list names `t::one`"),
            "{advice}"
        );
        assert!(advice.contains("another guard in the code"), "{advice}");
        let Verdict::Flagged(_, advice) = super::judge(&unnamed, &nothing, &tests) else {
            panic!("not flagged");
        };
        assert!(!advice.contains("another guard"), "{advice}");
    }

    #[test]
    fn the_work_is_done_in_this_order_and_each_break_is_compiled_before_any_is_run() {
        let mut run = Run::new("order");

        run.go(&breaks("demo", LIST)).unwrap();

        assert_eq!(
            run.runner.asked,
            [
                "test demo",
                "compile demo",
                "compile demo",
                "compile demo",
                "test demo",
                "test demo",
                "test demo"
            ]
        );
    }

    #[test]
    fn the_tree_is_only_read_and_the_copy_is_put_back_after_every_break() {
        let mut run = Run::new("untouched");
        let before = fs::metadata(run.root.join("crates/demo/src/lib.rs"))
            .unwrap()
            .modified()
            .unwrap();

        run.go(&breaks("demo", LIST)).unwrap();

        let library = run.root.join("crates/demo/src/lib.rs");
        assert_eq!(fs::read_to_string(&library).unwrap(), LIB);
        assert_eq!(fs::metadata(&library).unwrap().modified().unwrap(), before);
        assert_eq!(run.copy_of_the_library(), LIB);
        // Nothing of the run is in the tree.
        let mut entries: Vec<String> = fs::read_dir(&run.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, ["README.md", "crates"]);
    }

    #[test]
    fn a_run_out_of_time_carries_on_where_it_stopped() {
        let mut run = Run::new("resume");
        let list = breaks("demo", LIST);

        // Time for the tests with nothing broken, three compiles and one
        // break.
        let first = run.go_for(&list, 5).unwrap();

        assert_eq!(first, Finished::Unfinished { left: 2 });
        assert!(
            run.said.contains("Out of time with 2 piece(s)"),
            "{}",
            run.said
        );
        assert_eq!(run.runner.asked.len(), 5);

        let second = run.go(&list).unwrap();

        assert_eq!(second, Finished::Complete { flagged: 1 });
        // Two more pieces of work, and none of the first five again.
        assert_eq!(run.runner.asked.len(), 7);
        assert_eq!(run.runner.asked[5..], ["test demo", "test demo"]);
        assert!(
            run.said.contains("[2/3] demo/far-over-factor"),
            "{}",
            run.said
        );

        // A third run has nothing to do, and says the same.
        let third = run.go(&list).unwrap();
        assert_eq!(third, Finished::Complete { flagged: 1 });
        assert_eq!(run.runner.asked.len(), 7);
    }

    #[test]
    fn with_no_time_at_all_nothing_is_run() {
        let mut run = Run::new("no-time");

        let finished = run.go_for(&breaks("demo", LIST), 0).unwrap();

        assert_eq!(finished, Finished::Unfinished { left: 7 });
        assert_eq!(run.runner.asked, [] as [&str; 0]);
    }

    #[test]
    fn a_result_cut_short_by_a_kill_is_run_again_and_the_rest_are_kept() {
        let mut run = Run::new("torn");
        let list = breaks("demo", LIST);
        run.go(&list).unwrap();
        let results = run.scratch.join("results.jsonl");
        let text = fs::read_to_string(&results).unwrap();
        // The last line is the last break's result. Cut it in the middle.
        let cut = text.trim_end().len() - 20;
        fs::write(&results, &text[..cut]).unwrap();
        run.runner.asked.clear();

        let finished = run.go(&list).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert_eq!(run.runner.asked, ["test demo"]);
    }

    #[test]
    fn a_break_left_in_the_copy_by_a_killed_run_is_undone_first() {
        let mut run = Run::new("left-over");
        let list = breaks("demo", LIST);
        run.go_for(&list, 1).unwrap();
        fs::write(
            run.scratch.join("tree/crates/demo/src/lib.rs"),
            LIB.replace("seconds > budget", "true"),
        )
        .unwrap();

        // With the break still there the tests "with nothing broken" would
        // fail, and every break after it would be judged against it.
        let finished = run.go(&list).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert_eq!(run.copy_of_the_library(), LIB);
    }

    #[test]
    fn a_change_to_the_code_runs_everything_again_and_a_change_to_a_list_only_what_changed() {
        let mut run = Run::new("keys");
        let list = breaks("demo", LIST);
        run.go(&list).unwrap();
        run.runner.asked.clear();

        // A new break, and a new rule for an old one: one compile and one
        // run, for the new break only.
        let longer = format!(
            "{}\n[break] another\nrule: r\nfile: crates/demo/src/lib.rs\nfind: \"over\"\nreplace: \"above\"\n",
            LIST.replace("A tier at its budget is not over it.", "Reworded.")
        );
        fs::create_dir_all(run.root.join("mutations")).unwrap();
        fs::write(run.root.join("mutations/demo.breaks"), &longer).unwrap();
        run.go(&breaks("demo", &longer)).unwrap();
        assert_eq!(run.runner.asked, ["compile demo", "test demo"]);
        run.runner.asked.clear();

        // The code changes: nothing known is about this code any more.
        fs::write(run.root.join("README.md"), "A demo, changed.\n").unwrap();
        run.go(&list).unwrap();
        assert_eq!(run.runner.asked.len(), 7);
    }

    #[test]
    fn asking_again_forgets_the_chosen_breaks_and_no_others() {
        let mut run = Run::new("again");
        let list = breaks("demo", LIST);
        run.go(&list).unwrap();
        run.runner.asked.clear();
        let options = Options {
            again: true,
            compile_check: true,
        };

        drive(
            &run.root,
            &run.scratch,
            &list[1..2],
            &options,
            &mut run.runner,
            &mut || false,
            &mut |_| {},
        )
        .unwrap();

        // The tests with nothing broken are not forgotten: the code is the same.
        assert_eq!(run.runner.asked, ["compile demo", "test demo"]);
        run.runner.asked.clear();
        run.go(&list).unwrap();
        assert_eq!(run.runner.asked, [] as [&str; 0]);
    }

    #[test]
    fn a_break_that_does_not_compile_is_reported_before_any_break_is_run() {
        let mut run = Run::new("no-compile");
        let list = format!(
            "{LIST}\n[break] bad\nrule: r\nfile: crates/demo/src/lib.rs\nfind: \"over\"\nreplace: does not compile\n"
        );

        let finished = run.go_for(&breaks("demo", &list), 5).unwrap();

        // Out of time after the four compiles: no break has been run, and
        // the one that does not compile has been named.
        assert_eq!(finished, Finished::Unfinished { left: 3 });
        assert!(
            run.said
                .contains("does not compile: demo/bad: error[E0425]: cannot find value"),
            "{}",
            run.said
        );
        assert!(!run.said.contains("[1/4]"), "{}", run.said);

        let finished = run.go(&breaks("demo", &list)).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 2 });
        assert!(
            run.said.contains("DOES NOT COMPILE: demo/bad"),
            "{}",
            run.said
        );
        // It was never run.
        let tests = run
            .runner
            .asked
            .iter()
            .filter(|asked| asked.starts_with("test"))
            .count();
        assert_eq!(tests, 4);
    }

    #[test]
    fn without_the_compile_check_a_break_that_does_not_compile_is_still_not_caught() {
        let mut run = Run::new("no-check");
        let list = breaks(
            "demo",
            "[break] bad\nrule: r\nfile: crates/demo/src/lib.rs\nfind: \"over\"\nreplace: does not compile\n",
        );
        let options = Options {
            again: false,
            compile_check: false,
        };

        let finished = drive(
            &run.root,
            &run.scratch,
            &list,
            &options,
            &mut run.runner,
            &mut || false,
            &mut |line| run.said.push_str(line),
        )
        .unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert!(run.said.contains("DOES NOT COMPILE"), "{}", run.said);
        assert_eq!(run.runner.asked, ["test demo", "test demo"]);

        // It has no verdict, so the next run tries it again.
        run.runner.asked.clear();
        drive(
            &run.root,
            &run.scratch,
            &list,
            &options,
            &mut run.runner,
            &mut || false,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(run.runner.asked, ["test demo"]);
    }

    #[test]
    fn a_run_that_names_no_failed_test_is_not_a_break_that_was_caught() {
        let mut run = Run::new("broken");
        let list = breaks(
            "demo",
            "[break] odd\nrule: r\nfile: crates/demo/src/lib.rs\nfind: \"over\"\nreplace: \"breaks the run\"\n",
        );

        let finished = run.go(&list).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert!(
            run.said.contains("COULD NOT BE JUDGED: demo/odd"),
            "{}",
            run.said
        );
    }

    #[test]
    fn a_run_that_failed_without_a_verdict_is_tried_again_the_next_time() {
        let mut run = Run::new("lost");
        run.runner.runs_to_lose = 1;
        let list = breaks("demo", LIST);

        // The first break's run is lost. It is flagged, not called caught
        // and not called survived.
        let first = run.go(&list).unwrap();
        assert_eq!(first, Finished::Complete { flagged: 2 });
        assert!(
            run.said
                .contains("COULD NOT BE JUDGED: demo/over-is-strict"),
            "{}",
            run.said
        );
        run.runner.asked.clear();
        run.said.clear();

        // The next run tries that one again, and only that one.
        let second = run.go(&list).unwrap();
        assert_eq!(second, Finished::Complete { flagged: 1 });
        assert_eq!(run.runner.asked, ["test demo"]);
        assert!(
            run.said
                .contains("[1/3] demo/over-is-strict  caught by 1 test(s)"),
            "{}",
            run.said
        );
        run.runner.asked.clear();
        run.go(&list).unwrap();
        assert_eq!(run.runner.asked, [] as [&str; 0]);
    }

    #[test]
    fn a_break_that_did_not_compile_is_compiled_again_the_next_time() {
        let mut run = Run::new("recompiled");
        let list = breaks(
            "demo",
            "[break] bad\nrule: r\nfile: crates/demo/src/lib.rs\nfind: \"over\"\nreplace: does not compile\n",
        );
        assert_eq!(run.go(&list).unwrap(), Finished::Complete { flagged: 1 });
        run.runner.asked.clear();

        assert_eq!(run.go(&list).unwrap(), Finished::Complete { flagged: 1 });

        assert_eq!(run.runner.asked, ["compile demo"]);
    }

    #[test]
    fn a_second_run_on_the_same_copy_is_refused_while_the_first_holds_it() {
        let mut run = Run::new("locked");
        let list = breaks("demo", LIST);
        let held = super::lock(&run.scratch).unwrap();

        let error = run.go(&list).unwrap_err();

        assert!(
            error.contains("another `cargo xtask mutate` is running on the copy"),
            "{error}"
        );
        assert_eq!(run.runner.asked, [] as [&str; 0]);
        // Nothing of the copy was touched.
        assert!(!run.scratch.join("tree").exists());

        // A run that ends, or is killed, lets go.
        drop(held);
        assert_eq!(run.go(&list).unwrap(), Finished::Complete { flagged: 1 });
    }

    #[test]
    fn a_file_that_is_already_the_same_is_not_written_again() {
        let mut run = Run::new("unwritten");
        let list = breaks("demo", LIST);
        run.go_for(&list, 0).unwrap();
        let tree = run.scratch.join("tree");
        let modified = |file: &str| fs::metadata(tree.join(file)).unwrap().modified().unwrap();
        let (readme, manifest) = (modified("README.md"), modified("crates/demo/Cargo.toml"));
        fs::write(run.root.join("README.md"), "A demo, changed.\n").unwrap();

        run.go_for(&list, 0).unwrap();

        // The file that changed is the tree's now. The one that did not
        // keeps its time, and with it everything built from it.
        assert_eq!(
            fs::read_to_string(run.scratch.join("tree/README.md")).unwrap(),
            "A demo, changed.\n"
        );
        assert_eq!(modified("crates/demo/Cargo.toml"), manifest);
        assert!(modified("README.md") >= readme);
    }

    #[test]
    fn a_break_that_is_dropped_without_being_put_back_is_put_back() {
        let run = Run::new("dropped");
        let list = breaks("demo", LIST);
        let copy = run.root.clone();

        let broken = super::Broken::make(&copy, &list[0]).unwrap();
        assert_ne!(
            fs::read_to_string(copy.join("crates/demo/src/lib.rs")).unwrap(),
            LIB
        );
        // As when an error or a panic ends the run between the two.
        drop(broken);

        assert_eq!(
            fs::read_to_string(copy.join("crates/demo/src/lib.rs")).unwrap(),
            LIB
        );
    }

    #[test]
    fn a_test_that_fails_with_nothing_broken_stops_the_run() {
        let mut run = Run::new("baseline");
        run.runner.always_failing = Some("tests::unrelated");

        let error = run.go(&breaks("demo", LIST)).unwrap_err();

        assert!(
            error.contains("1 test(s) fail with nothing broken: demo tests::unrelated"),
            "{error}"
        );
        assert!(error.contains("cargo xtask stress"), "{error}");
        assert_eq!(run.runner.asked, ["test demo"]);
    }

    #[test]
    fn a_break_caught_only_by_other_tests_than_its_own_is_flagged() {
        let mut run = Run::new("missed");
        let list = breaks(
            "demo",
            "[break] wrong-test\nrule: r\nfile: crates/demo/src/lib.rs\nfind: 1.5 * budget\nreplace: 2.0 * budget\ncaught-by:\n    tests::at_the_budget_is_not_over\n    tests::far_over_is_over\n",
        );

        let finished = run.go(&list).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert!(
            run.said.contains(
                "NOT CAUGHT BY ITS OWN TESTS: demo/wrong-test  (mutations/demo.breaks:1)"
            ),
            "{}",
            run.said
        );
        assert!(
            run.said.contains(
                "Its list says `tests::at_the_budget_is_not_over` must fail, and with the \
                 break made it passed."
            ),
            "{}",
            run.said
        );
    }

    #[test]
    fn a_break_that_names_a_test_nothing_is_called_is_flagged() {
        let mut run = Run::new("renamed");
        let list = breaks(
            "demo",
            "[break] renamed\nrule: r\nfile: crates/demo/src/lib.rs\nfind: 1.5 * budget\nreplace: 2.0 * budget\ncaught-by:\n    tests::the_old_name\n",
        );

        let finished = run.go(&list).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert!(
            run.said
                .contains("NAMES A TEST THAT DOES NOT EXIST: demo/renamed"),
            "{}",
            run.said
        );
        assert!(
            run.said
                .contains("No test of its packages is called `tests::the_old_name`"),
            "{}",
            run.said
        );
    }

    #[test]
    fn a_break_expected_to_survive_passes_when_it_does_and_is_flagged_when_it_is_caught() {
        let mut run = Run::new("survives");
        let list = breaks(
            "demo",
            "[break] unseen\nrule: Nothing can see it.\nfile: crates/demo/src/lib.rs\nfind: \"within\"\nreplace: \"inside\"\nexpect: survives\n\n[break] seen\nrule: r\nfile: crates/demo/src/lib.rs\nfind: 1.5 * budget\nreplace: 2.0 * budget\nexpect: survives\n",
        );

        let finished = run.go(&list).unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 1 });
        assert!(
            run.said
                .contains("[1/2] demo/unseen  survived, as its list expects: no test failed"),
            "{}",
            run.said
        );
        assert!(
            run.said
                .contains("CAUGHT, AND EXPECTED TO SURVIVE: demo/seen"),
            "{}",
            run.said
        );
    }

    #[test]
    fn a_file_the_tree_no_longer_has_leaves_the_copy_with_its_directory() {
        let mut run = Run::new("removed");
        let list = breaks("demo", LIST);
        fs::create_dir_all(run.root.join("crates/old/src")).unwrap();
        fs::write(run.root.join("crates/old/src/lib.rs"), "pub fn old() {}\n").unwrap();
        run.go_for(&list, 0).unwrap();
        assert!(run.scratch.join("tree/crates/old/src/lib.rs").is_file());

        fs::remove_dir_all(run.root.join("crates/old")).unwrap();
        run.go_for(&list, 0).unwrap();

        assert!(!run.scratch.join("tree/crates/old").exists());
        assert!(run.scratch.join("tree/crates/demo/src/lib.rs").is_file());
    }

    #[test]
    fn a_build_directory_in_the_tree_is_not_copied() {
        let mut run = Run::new("target");
        fs::create_dir_all(run.root.join("target/debug")).unwrap();
        fs::write(run.root.join("target/debug/big"), "built").unwrap();
        fs::create_dir_all(run.root.join("fuzz/target")).unwrap();
        fs::write(run.root.join("fuzz/target/big"), "built").unwrap();

        run.go_for(&breaks("demo", LIST), 0).unwrap();

        assert!(!run.scratch.join("tree/target").exists());
        assert!(!run.scratch.join("tree/fuzz").exists());

        // What nextest leaves in the copy's own `target` stays there.
        fs::create_dir_all(run.scratch.join("tree/target/nextest/mutate")).unwrap();
        fs::write(
            run.scratch.join("tree/target/nextest/mutate/junit.xml"),
            "x",
        )
        .unwrap();
        run.go_for(&breaks("demo", LIST), 0).unwrap();
        assert!(
            run.scratch
                .join("tree/target/nextest/mutate/junit.xml")
                .is_file()
        );
        assert!(run.scratch.join("tree/README.md").is_file());
    }

    #[test]
    fn what_is_known_of_a_break_does_not_depend_on_which_breaks_it_was_run_with() {
        let mut run = Run::new("subset");
        fs::write(
            run.root.join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::create_dir_all(run.root.join("crates/other/src")).unwrap();
        fs::write(
            run.root.join("crates/other/src/lib.rs"),
            "pub fn other() -> u8 { 7 }\n",
        )
        .unwrap();
        let list = format!(
            "{LIST}\n[break] elsewhere\nrule: r\nfile: crates/other/src/lib.rs\nfind: 7\nreplace: 8\n"
        );
        let all = breaks("demo", &list);
        assert_eq!(run.go(&all).unwrap(), Finished::Complete { flagged: 2 });
        // One package's tests at a time with nothing broken, then the rest.
        assert_eq!(run.runner.asked[..2], ["test demo", "test other"]);
        run.runner.asked.clear();
        let mut said = String::new();

        // One break of the four, by itself: its tests are still known, so
        // it is still "caught", and nothing is run for it.
        let finished = report_only(&run.root, &run.scratch, &all[..1], &mut |line| {
            said.push_str(line);
        })
        .unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 0 }, "{said}");
        assert_eq!(
            run.go(&all[1..2]).unwrap(),
            Finished::Complete { flagged: 0 }
        );
        assert_eq!(run.runner.asked, [] as [&str; 0]);
    }

    #[test]
    fn a_break_is_judged_against_the_tests_of_the_packages_it_runs_together() {
        // `tests::only_with_other` is there only when `other` is in the
        // build, as a test behind a feature that another crate's tests
        // turn on is.
        struct Together(Pretend);
        impl Runner for Together {
            fn compiles(
                &mut self,
                tree: &Path,
                packages: &[String],
            ) -> Result<std::result::Result<(), String>> {
                self.0.compiles(tree, packages)
            }

            fn test(&mut self, tree: &Path, packages: &[String]) -> Result<Tested> {
                let mut tested = self.0.test(tree, packages)?;
                if let Tested::Ran(cases) = &mut tested
                    && packages.contains(&"other".to_owned())
                {
                    cases.push(TestCase {
                        binary: "demo".to_owned(),
                        name: "tests::only_with_other".to_owned(),
                        seconds: 0.01,
                        failed: !Pretend::library(tree).contains("1.5 * budget"),
                        retried: false,
                        repeat: None,
                    });
                }
                Ok(tested)
            }
        }
        let mut run = Run::new("together");
        let list = breaks(
            "demo",
            "[break] both\nrule: r\nfile: crates/demo/src/lib.rs\nfind: 1.5 * budget\nreplace: 2.0 * budget\npackages: other, demo\ncaught-by:\n    tests::only_with_other\n\n[break] alone\nrule: r\nfile: crates/demo/src/lib.rs\nfind: seconds > budget\nreplace: seconds >= budget\n\n[break] other-order\nrule: r\nfile: crates/demo/src/lib.rs\nfind: 1.5 * budget\nreplace: 3.0 * budget\npackages: demo, other\ncaught-by:\n    tests::only_with_other\n",
        );
        let options = Options {
            again: false,
            compile_check: false,
        };
        let mut runner = Together(Pretend::default());

        let finished = drive(
            &run.root,
            &run.scratch,
            &list,
            &options,
            &mut runner,
            &mut || false,
            &mut |line| {
                run.said.push_str(line);
                run.said.push('\n');
            },
        )
        .unwrap();

        // The test is known for the first break, which runs both packages,
        // and so it is "caught" and not "names a test that does not exist".
        assert_eq!(finished, Finished::Complete { flagged: 0 }, "{}", run.said);
        assert_eq!(
            runner.0.asked,
            // Nothing broken, once for each set of packages: the third
            // break's packages are the first's in another order, and are
            // not run again for it. Then the three breaks.
            [
                "test other,demo",
                "test demo",
                "test other,demo",
                "test demo",
                "test demo,other"
            ]
        );
    }

    #[test]
    fn a_report_is_of_what_is_known_and_runs_nothing() {
        let mut run = Run::new("report");
        let list = breaks("demo", LIST);
        run.go_for(&list, 5).unwrap();
        run.runner.asked.clear();
        let mut said = String::new();

        let finished = report_only(&run.root, &run.scratch, &list, &mut |line| {
            said.push_str(line);
        })
        .unwrap();

        assert_eq!(finished, Finished::Unfinished { left: 2 });
        assert!(
            said.contains("3 break(s): 1 did what the list expects, 0 flagged, 2 not run yet."),
            "{said}"
        );
        assert_eq!(run.runner.asked, [] as [&str; 0]);
    }

    /// The whole of it against the real cargo and nextest: the workspace in
    /// `xtask/fixtures/mutate`, with two breaks that are caught, one that
    /// nothing catches, one that is expected to survive and one that does
    /// not compile.
    ///
    /// It builds that workspace about ten times, which is seconds on Linux
    /// and too long for a blocking tier on Windows, so it is ignored: run
    /// it after a change to the driver, with
    /// `cargo nextest run -p xtask --run-ignored only sys_the_real`.
    #[test]
    #[ignore = "runs cargo about ten times; run it after changing the driver"]
    fn sys_the_real_cargo_and_nextest_catch_what_is_caught_and_flag_the_rest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/mutate");
        let library = root.join("crates/demo/src/lib.rs");
        let before = fs::read_to_string(&library).unwrap();
        let scratch = crate::workspace::metadata(&crate::workspace::root())
            .unwrap()
            .target_directory
            .join("tmp")
            .join("xtask-mutate-fixture");
        if scratch.exists() {
            fs::remove_dir_all(&scratch).unwrap();
        }
        fs::create_dir_all(&scratch).unwrap();
        let breaks = list::load_all(&root).unwrap();
        assert_eq!(breaks.len(), 5);
        assert_eq!(super::super::check(&root, &breaks), [] as [String; 0]);
        let options = Options {
            again: false,
            compile_check: true,
        };
        let mut said = String::new();

        let finished = drive(
            &root,
            &scratch,
            &breaks,
            &options,
            &mut CargoRunner::new(&scratch, 0),
            &mut || false,
            &mut |line| {
                said.push_str(line);
                said.push('\n');
            },
        )
        .unwrap();

        assert_eq!(finished, Finished::Complete { flagged: 2 }, "{said}");
        for expected in [
            "  5 tests pass.",
            "  does not compile: demo/not-rust: crates/demo/src/lib.rs:16:20: error: expected one of",
            "[1/5] demo/over-is-strict  caught by 1 test(s): demo \
             tests::at_the_budget_is_not_over  (",
            "[2/5] demo/far-over-factor  caught by 2 test(s): demo tests::far_over_is_over, \
             demo tests::the_line_is_one_and_a_half  (",
            "[3/5] demo/label-unseen  NOT CAUGHT  (",
            "[4/5] demo/sum-backwards  survived, as its list expects: no test failed  (",
            "5 break(s): 3 did what the list expects, 2 flagged.",
            "NOT CAUGHT: demo/label-unseen  (mutations/demo.breaks:23)",
            "DOES NOT COMPILE: demo/not-rust  (mutations/demo.breaks:37)",
        ] {
            assert!(said.contains(expected), "`{expected}` is not in:\n{said}");
        }
        // The break that does not compile was never run.
        assert!(!said.contains("[5/5]"), "{said}");

        // The fixture was only read: nothing was built in it, and the copy
        // is as it was before the first break.
        assert_eq!(fs::read_to_string(&library).unwrap(), before);
        assert!(!root.join("target").exists());
        assert_eq!(
            fs::read_to_string(scratch.join("tree/crates/demo/src/lib.rs")).unwrap(),
            before
        );

        // A second run says the same. It compiles the break that did not
        // compile once more, and runs no break's tests again.
        let runs = |results: &str| results.matches("\"status\":\"ran\"").count();
        let results = fs::read_to_string(scratch.join("results.jsonl")).unwrap();
        assert_eq!(runs(&results), 4);
        let again = drive(
            &root,
            &scratch,
            &breaks,
            &options,
            &mut CargoRunner::new(&scratch, 0),
            &mut || false,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(again, Finished::Complete { flagged: 2 });
        let after = fs::read_to_string(scratch.join("results.jsonl")).unwrap();
        assert_eq!(runs(&after), 4);
        assert!(after.starts_with(&results));

        // With no compile check, the break that does not compile is met in
        // its test run, and is still not a break that survived.
        let unchecked = Options {
            again: true,
            compile_check: false,
        };
        let mut said = String::new();
        let met_in_its_run = drive(
            &root,
            &scratch,
            &breaks[4..],
            &unchecked,
            &mut CargoRunner::new(&scratch, 0),
            &mut || false,
            &mut |line| said.push_str(line),
        )
        .unwrap();
        assert_eq!(met_in_its_run, Finished::Complete { flagged: 1 }, "{said}");
        assert!(
            said.contains("[1/1] demo/not-rust  DOES NOT COMPILE"),
            "{said}"
        );

        // Two jobs, from nothing: the same verdicts, each job in a copy
        // and a build directory of its own.
        fs::remove_dir_all(&scratch).unwrap();
        fs::create_dir_all(&scratch).unwrap();
        let mut first = CargoRunner::new(&scratch, 0);
        let mut second = CargoRunner::new(&scratch, 1);
        let mut said = String::new();
        let two_jobs = super::drive_jobs(
            &root,
            &scratch,
            &breaks,
            &options,
            &mut [&mut first, &mut second],
            &mut || false,
            &mut |line| {
                said.push_str(line);
                said.push('\n');
            },
        )
        .unwrap();
        assert_eq!(two_jobs, Finished::Complete { flagged: 2 }, "{said}");
        for expected in [
            "demo/over-is-strict  caught by 1 test(s)",
            "demo/far-over-factor  caught by 2 test(s)",
            "demo/label-unseen  NOT CAUGHT",
            "demo/sum-backwards  survived, as its list expects",
            "5 break(s): 3 did what the list expects, 2 flagged.",
        ] {
            assert!(said.contains(expected), "`{expected}` is not in:\n{said}");
        }
        for copy in ["tree", "tree-2"] {
            assert_eq!(
                fs::read_to_string(scratch.join(copy).join("crates/demo/src/lib.rs")).unwrap(),
                before,
                "{copy}"
            );
        }
        assert!(scratch.join("target-2").is_dir(), "the second job's build");
    }
}
