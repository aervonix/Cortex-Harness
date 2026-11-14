//! One test run again in a child process with an environment of its own: a
//! `PATH`, a working directory and variables (`docs/testing-and-ci.md`,
//! "A test in a process of its own").
//!
//! How the harness finds a program depends on the process's `PATH` and its
//! working directory, which a test cannot change: the tests of one binary
//! share them, and nextest runs several at once. So a test of the search
//! reruns itself:
//!
//! ```ignore
//! #[test]
//! fn sys_a_relative_path_entry_is_passed_over() {
//!     if rerun::is_child() {
//!         rerun::say("found", &format!("{:?}", search_the_way_the_harness_does()));
//!         return;
//!     }
//!     let ran = Rerun::this_test().path(["node_modules/.bin", "/usr/bin"]).cwd(&ws).run();
//!     assert_eq!(ran.said("found"), Some("…"));
//! }
//! ```
//!
//! The child is the same test binary, running only that test, with
//! `CORTEX_TESTKIT_RERUN` set so that the test knows which half it is. What
//! the child reports it says with [`say`], one line a key, and the parent
//! reads it back with [`Ran::said`]. No helper program is built.

#![allow(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    reason = "test support: it starts the test binary as a child and waits for it"
)]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The variable that tells a test it is the child.
const CHILD: &str = "CORTEX_TESTKIT_RERUN";

/// What starts a line [`say`] writes.
const SAID: &str = "cx-rerun:";

/// Whether this process is a test run again by [`Rerun`].
#[must_use]
pub fn is_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

/// Says `value` under `key`, for the parent to read with [`Ran::said`]. A
/// value is one line: a line break in it is written as `\n`.
#[expect(
    clippy::print_stdout,
    reason = "the child tells the parent through its standard output"
)]
pub fn say(key: &str, value: &str) {
    println!("{SAID}{key}={}", value.replace('\n', "\\n"));
}

/// A test to run again in a child process, and the environment it gets.
#[derive(Debug)]
pub struct Rerun {
    test: String,
    path: Option<OsString>,
    cwd: Option<PathBuf>,
    variables: Vec<(OsString, Option<OsString>)>,
}

impl Rerun {
    /// The test that is running on this thread, by the name the test
    /// harness gives its thread (`module::test`).
    ///
    /// # Panics
    ///
    /// On a thread that is not a test's own.
    #[must_use]
    pub fn this_test() -> Self {
        let name = std::thread::current()
            .name()
            .filter(|name| *name != "main")
            .expect("Rerun::this_test is called on a test's own thread")
            .to_owned();
        Self::test(&name)
    }

    /// The test called `name` (`module::test`), in this test binary.
    #[must_use]
    pub fn test(name: &str) -> Self {
        Self {
            test: name.to_owned(),
            path: None,
            cwd: None,
            variables: Vec::new(),
        }
    }

    /// The child's `PATH`: these entries, in order, joined as the system
    /// joins them. A relative entry stays relative.
    ///
    /// # Panics
    ///
    /// When an entry holds the system's separator of entries.
    #[must_use]
    pub fn path<I, P>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<OsStr>,
    {
        let entries: Vec<OsString> = entries
            .into_iter()
            .map(|entry| entry.as_ref().to_owned())
            .collect();
        self.path = Some(std::env::join_paths(entries).expect("PATH entries without a separator"));
        self
    }

    /// The child's working directory.
    #[must_use]
    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_owned());
        self
    }

    /// Sets a variable in the child's environment.
    #[must_use]
    pub fn env(mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.variables
            .push((name.as_ref().to_owned(), Some(value.as_ref().to_owned())));
        self
    }

    /// Takes a variable out of the child's environment.
    #[must_use]
    pub fn env_remove(mut self, name: impl AsRef<OsStr>) -> Self {
        self.variables.push((name.as_ref().to_owned(), None));
        self
    }

    /// The command that runs the child: the test binary, running only this
    /// test, with its output captured.
    ///
    /// # Panics
    ///
    /// When the test binary's path cannot be known.
    #[must_use]
    pub fn command(&self) -> Command {
        let mut command =
            Command::new(std::env::current_exe().expect("the test binary has a path"));
        command
            .args([
                self.test.as_str(),
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        for (name, value) in &self.variables {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
    }

    /// Runs the child to its end.
    ///
    /// # Panics
    ///
    /// When the child cannot be started, or the test failed or did not run
    /// in it: the panic has what it printed.
    #[must_use]
    pub fn run(&self) -> Ran {
        let output = self
            .command()
            .output()
            .unwrap_or_else(|error| panic!("could not rerun {}: {error}", self.test));
        let ran = Ran {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        assert!(
            output.status.success(),
            "the rerun of {} failed ({}):\n{}\n{}",
            self.test,
            output.status,
            ran.stdout,
            ran.stderr
        );
        // A name that matches no test runs nothing, and passes.
        assert!(
            ran.stdout.contains("running 1 test"),
            "the rerun of {} ran no test: is that its whole name?\n{}",
            self.test,
            ran.stdout
        );
        ran
    }
}

/// What a child printed.
#[derive(Debug, Clone)]
pub struct Ran {
    /// Its standard output, the lines of [`say`] among the test harness's.
    pub stdout: String,
    /// Its standard error.
    pub stderr: String,
}

impl Ran {
    /// What the child said under `key` with [`say`], the last time it did.
    #[must_use]
    pub fn said(&self, key: &str) -> Option<String> {
        let prefix = format!("{SAID}{key}=");
        // Anywhere in a line: the test harness writes `test name ... ` and
        // no line break before the test's own output.
        self.stdout
            .lines()
            .filter_map(|line| line.split_once(&prefix).map(|(_, value)| value))
            .next_back()
            .map(|value| value.replace("\\n", "\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::{Rerun, is_child, say};

    /// The child sees the `PATH`, the directory and the variables it was
    /// given, and the parent reads what it said.
    #[test]
    fn sys_a_rerun_test_gets_the_environment_it_is_given() {
        if is_child() {
            say("path", &std::env::var("PATH").unwrap_or_default());
            say("cwd", &std::env::current_dir().unwrap().to_string_lossy());
            say("set", &std::env::var("CX_RERUN_SET").unwrap_or_default());
            say(
                "removed",
                &format!("{:?}", std::env::var_os("CX_RERUN_REMOVED")),
            );
            say("lines", "one\ntwo");
            return;
        }
        let dir = std::env::temp_dir();
        let ran = Rerun::this_test()
            .path(["relative/bin", "/usr/bin"])
            .cwd(&dir)
            .env("CX_RERUN_SET", "yes")
            .env_remove("CX_RERUN_REMOVED")
            .run();
        let joined = std::env::join_paths(["relative/bin", "/usr/bin"]).unwrap();
        assert_eq!(ran.said("path").as_deref(), joined.to_str());
        let cwd = std::path::PathBuf::from(ran.said("cwd").unwrap());
        assert_eq!(cwd.canonicalize().unwrap(), dir.canonicalize().unwrap());
        assert_eq!(ran.said("set").as_deref(), Some("yes"));
        assert_eq!(ran.said("removed").as_deref(), Some("None"));
        assert_eq!(ran.said("lines").as_deref(), Some("one\ntwo"));
        assert_eq!(ran.said("never"), None);
    }

    /// A name that is not a test's whole name runs nothing, which would
    /// pass: it is refused.
    #[test]
    fn sys_a_rerun_of_a_test_that_is_not_there_fails() {
        if is_child() {
            return;
        }
        let failed = std::panic::catch_unwind(|| Rerun::test("rerun::tests::no_such_test").run());
        let message = failed.expect_err("a rerun that ran nothing passed");
        let text = message
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(text.contains("ran no test"), "{text}");
    }
}
