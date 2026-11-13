//! Integration tests (tier 2): the tools, run as the loop will run them, on
//! a real file system.
//!
//! Each test has a workspace of its own under Cargo's temporary directory
//! for this crate, removed when the test ends. A call goes through
//! `Tool::plan` and `execute`, on a single-threaded runtime, so every result
//! here has passed the output policy.
//!
//! The workspace helper lives here for now. The test kit takes it over when
//! it can (`docs/testing-and-ci.md`).

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "an integration test prepares real files, fails by panicking, and says what it measured"
)]

mod execute;
mod list;
#[cfg(unix)]
mod paths_unix;
#[cfg(windows)]
mod paths_windows;
mod perf;
mod read;
mod registry;
mod search;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cortex_base::{PathOptions, PathResolver};
use cortex_tools::{
    FileTable, Json, MemoryStore, NoOutput, Plan, RunCx, Tool, ToolError, ToolResult, execute,
};

/// A workspace that belongs to one test and is removed with it.
struct Workspace {
    /// The test's own directory. The workspace is `ws` inside it, so that a
    /// test can also put things beside the workspace.
    dir: PathBuf,
    paths: Arc<PathResolver>,
    store: MemoryStore,
    files: Arc<FileTable>,
}

impl Workspace {
    /// A fresh, empty workspace named after the test.
    fn new(test: &str) -> Self {
        Self::with_options(test, PathOptions::new())
    }

    fn with_options(test: &str, options: PathOptions) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("cortex-tools-it")
            .join(test);
        if dir.exists() {
            remove_all(&dir);
        }
        let root = dir.join("ws");
        fs::create_dir_all(&root).expect("create the workspace");
        let paths = PathResolver::new(&root, options).expect("open the workspace");
        Self {
            dir,
            paths: Arc::new(paths),
            store: MemoryStore::new(),
            files: Arc::new(FileTable::new()),
        }
    }

    /// The workspace root, as the tests write files into it.
    fn root(&self) -> PathBuf {
        self.dir.join("ws")
    }

    /// Writes a file, and its parent directories, inside the workspace.
    fn file(&self, relative: &str, contents: &str) -> PathBuf {
        self.bytes(relative, contents.as_bytes())
    }

    /// The same, for content that is not text.
    fn bytes(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the file's directory");
        }
        fs::write(&path, contents).expect("write a file");
        path
    }

    /// Creates a directory, and its parents, inside the workspace.
    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root().join(relative);
        fs::create_dir_all(&path).expect("create a directory");
        path
    }

    /// What a call runs with: this workspace's resolver, store and table of
    /// seen files, and the default limits.
    fn cx(&self) -> RunCx {
        RunCx::new(
            Arc::clone(&self.paths),
            Arc::new(self.store.clone()),
            Arc::clone(&self.files),
        )
    }

    /// Plans a call.
    fn plan(&self, tool: &dyn Tool, args: Json) -> Result<Plan, ToolError> {
        tool.plan(&args, &self.cx().plan_cx())
    }

    /// Plans and runs a call with the default context. An error from
    /// planning becomes the result, as it does in the loop.
    fn call(&self, tool: &dyn Tool, args: Json) -> ToolResult {
        self.call_with(&self.cx(), tool, args)
    }

    /// Plans and runs a call with a context the test prepared.
    fn call_with(&self, cx: &RunCx, tool: &dyn Tool, args: Json) -> ToolResult {
        let plan = match tool.plan(&args, &cx.plan_cx()) {
            Ok(plan) => plan,
            Err(error) => return error.into_result(),
        };
        let result = block_on(execute(tool, plan, cx, &mut NoOutput));
        assert!(
            result.is_bounded(),
            "a result that skipped the output policy"
        );
        assert!(
            cx.limits.fits(&result.text()),
            "a result over the limits:\n{}",
            result.text()
        );
        result
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        remove_all(&self.dir);
    }
}

/// Runs a future to its end on a runtime of its own, as the core thread
/// would: one thread, with the blocking pool behind it.
fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime")
        .block_on(future)
}

/// The text of a result that must have succeeded.
#[track_caller]
fn ok(result: &ToolResult) -> String {
    assert!(!result.is_error(), "the call failed: {}", result.text());
    result.text()
}

/// The text of a result that must have failed.
#[track_caller]
fn failed(result: &ToolResult) -> String {
    assert_eq!(
        result.outcome(),
        &cortex_protocol::item::ToolOutcome::Error,
        "the call did not fail: {}",
        result.text()
    );
    result.text()
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
