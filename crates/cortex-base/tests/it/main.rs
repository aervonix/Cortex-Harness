//! Integration tests (tier 2): the primitives that touch the file system,
//! on a real one.
//!
//! Each test works in a directory of its own under Cargo's temporary
//! directory for this crate, removed when the test ends. No test waits:
//! time comes from the fake clock, and file times are set, not waited for.
//!
//! The modules compiled only on Windows are the Windows coverage that
//! `docs/testing-and-ci.md` lists for paths and for files in use.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "an integration test prepares real files, fails by panicking, and says when it skips"
)]

mod atomic;
mod nofollow;
mod paths;
#[cfg(unix)]
mod paths_unix;
#[cfg(windows)]
mod paths_windows;
mod private;
mod watch;

use std::fs;
use std::path::{Path, PathBuf};

use cortex_base::{AbsPath, PathOptions, PathResolver};

/// A directory that belongs to one test and is removed with it.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// A fresh, empty directory named after the test.
    fn new(test: &str) -> Self {
        Self::under(Path::new(env!("CARGO_TARGET_TMPDIR")), test)
    }

    /// The same, under another parent: for a test that needs a particular
    /// volume.
    fn under(parent: &Path, test: &str) -> Self {
        let dir = parent.join("cortex-base-it").join(test);
        if dir.exists() {
            remove_all(&dir);
        }
        fs::create_dir_all(&dir).expect("create the test's directory");
        Self { dir }
    }

    /// A path inside the directory.
    fn join(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    /// Creates a directory, and its parents, inside the scratch directory.
    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.join(relative);
        fs::create_dir_all(&path).expect("create a directory");
        path
    }

    /// Creates a file, and its parents, inside the scratch directory.
    fn file(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the file's directory");
        }
        fs::write(&path, contents).expect("write a file");
        path
    }

    /// A resolver whose workspace is `relative` inside the scratch directory.
    fn resolver(&self, relative: &str) -> PathResolver {
        self.resolver_with(relative, PathOptions::new())
    }

    fn resolver_with(&self, relative: &str, options: PathOptions) -> PathResolver {
        PathResolver::new(&self.dir(relative), options).expect("open the workspace")
    }

    /// The real location of a path inside the scratch directory, as the
    /// resolver would report it.
    fn real(&self, relative: &str) -> AbsPath {
        let path = fs::canonicalize(self.join(relative)).expect("the path exists");
        AbsPath::from_os(&path).expect("the path reads back")
    }

    /// The names in a directory, sorted.
    fn listing(&self, relative: &str) -> Vec<String> {
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
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove_all(&self.dir);
    }
}

/// Removes a directory tree, read-only files included. Failure is ignored:
/// the next run of the test starts by removing it again.
fn remove_all(dir: &Path) {
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
