//! A directory of a test's own: a workspace with files in it, a git
//! repository when the test needs one, and nothing left behind.
//!
//! Every crate's tests had grown a copy of this. A test gets one with the
//! [`scratch!`](crate::scratch!) macro, which puts it under the directory
//! Cargo gives the crate's integration tests:
//!
//! ```text
//! let scratch = cortex_testkit::scratch!("a_link_is_refused");
//! let file = scratch.file("ws/src/main.rs", "fn main() {}\n");
//! scratch.git_init("ws");
//! ```
//!
//! - **It is new each time.** What an earlier run left under the same name
//!   is removed first, read-only files included.
//! - **It is removed when the test ends,** unless the test failed: then it
//!   is left for whoever looks at the failure, and the next run removes it.
//! - **It is inside the build directory,** so on a developer's machine it is
//!   inside the repository's checkout. A test that needs "no repository
//!   above this directory" asks for [`Scratch::outside`], which is in the
//!   system's temporary directory. A unit test, which Cargo gives no
//!   directory, uses that too.
//! - **Its git is the test's own.** [`git`] runs with no system or user
//!   configuration, a made-up author, no signing and no line-ending
//!   conversion, so the result is the same on every machine.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "test support that prepares real directories and starts git, from a test's own thread"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The variable that moves every [`scratch!`](crate::scratch!) directory
/// to another parent: for running the tests on a particular disk.
pub const BASE: &str = "CORTEX_TEST_TMPDIR";

/// A directory that belongs to one test. See the module's description.
#[derive(Debug)]
pub struct Scratch {
    dir: PathBuf,
    /// A parent that was made for this directory alone, and goes with it.
    own_parent: Option<PathBuf>,
}

/// A [`Scratch`] for the test named, under the directory Cargo gives this
/// crate's integration tests (or under [`BASE`](crate::scratch::BASE) when
/// that is set), in a folder named after the crate.
///
/// Only an integration test can use it: Cargo gives a unit test no
/// directory. A unit test uses [`Scratch::outside`](crate::scratch::Scratch::outside).
#[macro_export]
macro_rules! scratch {
    ($test:expr) => {
        $crate::scratch::Scratch::for_test(
            env!("CARGO_TARGET_TMPDIR"),
            concat!(env!("CARGO_PKG_NAME"), "-it"),
            $test,
        )
    };
}

impl Scratch {
    /// What [`scratch!`](crate::scratch!) calls: `<base>/<group>/<test>`,
    /// where the base is [`BASE`] when it is set and `default_base` when it
    /// is not.
    #[must_use]
    pub fn for_test(default_base: &str, group: &str, test: &str) -> Self {
        Self::under(&base(default_base, std::env::var_os(BASE)), group, test)
    }

    /// A new, empty directory at `<parent>/<group>/<test>`.
    ///
    /// # Panics
    ///
    /// When `group` or `test` is not one plain name. Whatever is at the path
    /// is removed first, so a name that is empty, or `.`, or a path of its
    /// own would remove another test's directory, or something that is no
    /// test's at all.
    #[must_use]
    pub fn under(parent: &Path, group: &str, test: &str) -> Self {
        for name in [group, test] {
            assert!(
                is_one_name(name),
                "`{name}` is not a name for a test's directory: it must be one path component, \
                 and not `.` or `..`"
            );
        }
        let dir = parent.join(group).join(test);
        if dir.exists() {
            remove_all(&dir);
        }
        fs::create_dir_all(&dir).expect("create the test's directory");
        Self {
            dir,
            own_parent: None,
        }
    }

    /// A new, empty directory in the system's temporary directory: outside
    /// the build directory, and so outside any repository. The process id
    /// is in its name, so two runs at once do not share it.
    #[must_use]
    pub fn outside(group: &str, test: &str) -> Self {
        let group = format!("{group}-{}", std::process::id());
        let mut scratch = Self::under(&std::env::temp_dir(), &group, test);
        scratch.own_parent = Some(std::env::temp_dir().join(group));
        scratch
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// A path inside the directory. Nothing is created.
    #[must_use]
    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.dir.join(relative)
    }

    /// Creates a directory, and its parents, inside the directory.
    pub fn dir(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.join(relative);
        fs::create_dir_all(&path).expect("create a directory");
        path
    }

    /// Writes a file, creating the directories it is in.
    pub fn file(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the file's directory");
        }
        fs::write(&path, contents).expect("write a file");
        path
    }

    /// The bytes of a file.
    #[must_use]
    pub fn read(&self, relative: impl AsRef<Path>) -> Vec<u8> {
        let path = self.join(relative);
        fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    /// The text of a file.
    #[must_use]
    pub fn read_text(&self, relative: impl AsRef<Path>) -> String {
        String::from_utf8(self.read(relative)).expect("the file is text")
    }

    /// The names in a directory, sorted.
    #[must_use]
    pub fn listing(&self, relative: impl AsRef<Path>) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.join(relative))
            .expect("list the directory")
            .map(|entry| {
                entry
                    .expect("read an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// Every file under a directory, at any depth, as paths relative to it
    /// with `/`, sorted. A link is listed and not followed.
    #[must_use]
    pub fn tree(&self, relative: impl AsRef<Path>) -> Vec<String> {
        fn walk(dir: &Path, prefix: &str, found: &mut Vec<String>) {
            for entry in fs::read_dir(dir).expect("list a directory") {
                let entry = entry.expect("read an entry");
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    walk(&entry.path(), &path, found);
                } else {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.join(relative), "", &mut found);
        found.sort();
        found
    }

    /// Makes a directory inside this one a git repository, with a first
    /// branch called `main`, and returns it. The directory is created if it
    /// is not there. An empty `relative` makes the whole directory one.
    ///
    /// A test that calls this starts a process, so it is a system test and
    /// its name starts with `sys_`.
    pub fn git_init(&self, relative: impl AsRef<Path>) -> PathBuf {
        let repository = self.dir(relative);
        git(&repository, &["init", "--quiet", "--initial-branch=main"]);
        repository
    }

    /// Stages everything in a repository inside this directory and commits
    /// it. Returns the commit's hash.
    pub fn git_commit_all(&self, relative: impl AsRef<Path>, message: &str) -> String {
        let repository = self.join(relative);
        git(&repository, &["add", "--all"]);
        git(&repository, &["commit", "--quiet", "--message", message]);
        git(&repository, &["rev-parse", "HEAD"])
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // What a failed test left is the evidence. The next run of the test
        // removes it.
        if !std::thread::panicking() {
            remove_all(&self.dir);
            // The parent too, once the last test that shares it is done:
            // removing a directory that is not empty fails, and is left.
            if let Some(parent) = &self.own_parent {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

/// The parent of a test's directory: where the variable says, or where
/// Cargo does.
fn base(default_base: &str, variable: Option<std::ffi::OsString>) -> PathBuf {
    variable
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(default_base), PathBuf::from)
}

/// Whether `name` is one ordinary path component.
fn is_one_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(only)), None) if only == name
    ) && !name.contains(['/', '\\', ':'])
        // Windows takes the dots and spaces off the end of a name: `x.` is
        // `x` there, and `...` is the directory itself.
        && !name.ends_with(['.', ' '])
}

/// Runs git in `directory` and returns what it printed, trimmed. Panics,
/// with git's own message, when git fails.
///
/// It reads no system or user configuration, and sets what a test would
/// otherwise take from the machine: an author, no signing, `main` as the
/// first branch, and no conversion of line endings. It also drops the
/// variables that point git at a repository other than the one in
/// `directory`, which are set when the tests are started from a git hook.
pub fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = git_command(directory, arguments)
        .output()
        .expect("git is installed where these tests run");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The variables that make git work on a repository other than the one it
/// is started in.
const ELSEWHERE: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
];

/// git, as [`git`] runs it.
fn git_command(directory: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new("git");
    for variable in ELSEWHERE {
        command.env_remove(variable);
    }
    command
        .current_dir(directory)
        .args([
            "-c",
            "user.name=Cortex Test",
            "-c",
            "user.email=test@cortex-harness.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.autocrlf=false",
        ])
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("no-such-git-config"))
        .env("GIT_AUTHOR_NAME", "Cortex Test")
        .env("GIT_AUTHOR_EMAIL", "test@cortex-harness.invalid")
        .env("GIT_COMMITTER_NAME", "Cortex Test")
        .env("GIT_COMMITTER_EMAIL", "test@cortex-harness.invalid");
    command
}

/// Removes a directory tree, read-only files included. A failure is
/// ignored: the next run of the test starts by removing it again.
pub fn remove_all(dir: &Path) {
    fn make_writable(path: &Path) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "a test file in a test directory, about to be deleted"
            )]
            permissions.set_readonly(false);
            let _ = fs::set_permissions(path, permissions);
        }
        if metadata.is_dir()
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
    make_writable(dir);
    let _ = fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::Scratch;

    fn scratch(test: &str) -> Scratch {
        Scratch::outside("cortex-testkit-scratch", test)
    }

    #[test]
    fn it_is_empty_and_what_an_earlier_run_left_is_gone() {
        let first = scratch("fresh");
        first.file("left/behind.txt", "old");
        let path = first.path().to_path_buf();
        // As a run that was killed leaves it: the directory, and no drop.
        std::mem::forget(first);
        assert!(path.join("left/behind.txt").is_file());

        let second = scratch("fresh");

        assert_eq!(second.path(), path);
        assert_eq!(second.listing(""), [] as [&str; 0]);
    }

    #[test]
    fn files_and_directories_are_made_with_their_parents_and_read_back() {
        let scratch = scratch("files");

        let file = scratch.file("ws/src/main.rs", "fn main() {}\n");
        let bytes = scratch.file("ws/data.bin", [0u8, 159, 146, 150]);
        let dir = scratch.dir("ws/empty/deep");

        assert_eq!(file, scratch.join("ws/src/main.rs"));
        assert!(dir.is_dir());
        assert_eq!(scratch.read_text("ws/src/main.rs"), "fn main() {}\n");
        assert_eq!(scratch.read("ws/data.bin"), [0u8, 159, 146, 150]);
        assert_eq!(fs::read(bytes).unwrap(), [0u8, 159, 146, 150]);
        assert_eq!(scratch.listing("ws"), ["data.bin", "empty", "src"]);
        assert_eq!(scratch.tree("ws"), ["data.bin", "src/main.rs"]);
        assert_eq!(scratch.tree(""), ["ws/data.bin", "ws/src/main.rs"]);
    }

    #[test]
    fn it_is_removed_when_the_test_ends_read_only_files_included() {
        let scratch = scratch("removed");
        let file = scratch.file("locked/file.txt", "x");
        // The file, then the directory it is in: each keeps its own mode
        // and loses only the right to be written.
        for path in [file, scratch.join("locked")] {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&path, permissions).unwrap();
        }
        let path = scratch.path().to_path_buf();

        drop(scratch);

        assert!(!path.exists());
    }

    #[test]
    fn it_is_left_where_it_is_when_the_test_fails() {
        // A panic that carries the directory's path out, in place of a
        // failed assertion.
        let panic = std::panic::catch_unwind(|| {
            let scratch = scratch("kept");
            scratch.file("evidence.txt", "what the test saw");
            std::panic::panic_any(scratch.path().to_path_buf());
        })
        .unwrap_err();
        let path: PathBuf = *panic.downcast::<PathBuf>().unwrap();

        assert!(path.join("evidence.txt").is_file());
        // The next run of the same test starts clean.
        assert_eq!(scratch("kept").listing(""), [] as [&str; 0]);
    }

    #[test]
    fn a_name_that_is_not_one_name_is_refused_before_anything_is_removed() {
        let parent = scratch("names");
        parent.file("group/other-test/kept.txt", "another test's file");
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "/tmp",
            "C:",
            "C:x",
            "../x",
            "...",
            "other-test.",
            "other-test ",
        ] {
            for (group, test) in [("group", name), (name, "test")] {
                let refused = std::panic::catch_unwind(|| {
                    let _ = Scratch::under(parent.path(), group, test);
                });
                assert!(refused.is_err(), "group {group:?}, test {test:?}");
            }
        }

        assert_eq!(
            parent.read_text("group/other-test/kept.txt"),
            "another test's file"
        );
    }

    #[test]
    fn git_is_started_with_the_tests_own_settings_and_no_repository_from_outside() {
        let directory = Path::new("somewhere");
        let command = super::git_command(directory, &["status"]);
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        let variable = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };

        // What the machine would otherwise decide.
        for setting in [
            "commit.gpgsign=false",
            "core.autocrlf=false",
            "init.defaultBranch=main",
            "user.email=test@cortex-harness.invalid",
        ] {
            assert!(
                arguments.contains(&setting.to_owned()),
                "{setting}: {arguments:?}"
            );
        }
        assert_eq!(arguments.last().map(String::as_str), Some("status"));
        assert_eq!(variable("GIT_CONFIG_NOSYSTEM"), Some(Some("1".to_owned())));
        assert_eq!(
            variable("GIT_CONFIG_GLOBAL"),
            Some(Some(
                directory
                    .join("no-such-git-config")
                    .to_string_lossy()
                    .into_owned()
            ))
        );
        // Removed, whatever the caller's environment holds.
        for elsewhere in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            assert_eq!(variable(elsewhere), Some(None), "{elsewhere}");
        }
    }

    #[test]
    fn the_variable_moves_the_parent_and_an_empty_one_does_not() {
        use std::ffi::OsString;

        assert_eq!(super::base("target/tmp", None), PathBuf::from("target/tmp"));
        assert_eq!(
            super::base("target/tmp", Some(OsString::from("/mnt/fast"))),
            PathBuf::from("/mnt/fast")
        );
        assert_eq!(
            super::base("target/tmp", Some(OsString::new())),
            PathBuf::from("target/tmp")
        );
    }

    #[test]
    fn a_directory_outside_leaves_no_parent_behind_once_the_last_is_gone() {
        let first = Scratch::outside("cortex-testkit-own-parent", "first");
        let second = Scratch::outside("cortex-testkit-own-parent", "second");
        let parent = first.path().parent().unwrap().to_path_buf();

        drop(first);
        assert!(parent.is_dir(), "another test's directory is still in it");
        drop(second);

        assert!(!parent.exists());
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_directory_changes_nothing_that_a_link_in_it_leads_to() {
        use std::os::unix::fs::PermissionsExt;

        let outside = scratch("link-target");
        let target = outside.file("kept.txt", "not the test's own");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
        let inside = scratch("link-holder");
        std::os::unix::fs::symlink(&target, inside.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), inside.join("link-to-directory")).unwrap();
        let path = inside.path().to_path_buf();

        drop(inside);

        assert!(!path.exists());
        // Still there, and still read-only: nothing was made writable
        // through the link.
        assert_eq!(fs::read_to_string(&target).unwrap(), "not the test's own");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }

    #[test]
    fn two_tests_of_one_group_do_not_share_a_directory() {
        let a = scratch("one");
        let b = scratch("other");

        a.file("x", "a");

        assert_ne!(a.path(), b.path());
        assert_eq!(b.listing(""), [] as [&str; 0]);
    }

    #[test]
    fn the_macro_puts_it_where_cargo_says_or_where_the_variable_says() {
        let base = scratch("for-test-base");

        let default = Scratch::for_test(base.path().to_str().unwrap(), "cortex-demo-it", "t");

        // Without the variable. With it set the parent is the variable's,
        // which is read from the environment and so is not set here.
        if std::env::var_os(super::BASE).is_none() {
            assert_eq!(default.path(), base.join("cortex-demo-it").join("t"));
        }
    }

    #[test]
    fn sys_a_repository_is_made_and_committed_to_whatever_the_machines_git_is_set_to() {
        let scratch = scratch("git");

        let repository = scratch.git_init("ws");
        scratch.file("ws/a.txt", "one\r\ntwo\n");
        let commit = scratch.git_commit_all("ws", "The first commit");

        assert_eq!(repository, scratch.join("ws"));
        assert_eq!(commit.len(), 40, "{commit}");
        assert_eq!(
            super::git(&repository, &["log", "--format=%an <%ae> %s"]),
            "Cortex Test <test@cortex-harness.invalid> The first commit"
        );
        assert_eq!(
            super::git(&repository, &["branch", "--show-current"]),
            "main"
        );
        // The line endings went in as they were written.
        assert_eq!(
            super::git(&repository, &["cat-file", "blob", "HEAD:a.txt"]),
            "one\r\ntwo"
        );
        assert_eq!(super::git(&repository, &["status", "--porcelain"]), "");
    }
}
