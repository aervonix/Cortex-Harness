//! `write` and `edit` on Windows: files another program holds open, the
//! read-only attribute, and paths in another spelling
//! (`docs/testing-and-ci.md`, "Windows coverage": "Files in use", "Line
//! endings"; `docs/architecture/05-tools.md`, "Atomic writes").
//!
//! Nothing here waits: the retries of a replace are waits on the fake
//! clock.

use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cortex_base::{Clock, ContentHash, FakeClock, UtcTime};
use cortex_tools::{EditTool, Editing, ReadTool, Staleness, WriteTool};
use serde_json::json;

use crate::{Workspace, failed, ok};

/// `FILE_SHARE_READ`: others may read the file while the handle is open,
/// and may not write, rename or delete it.
const SHARE_READ: u32 = 1;

/// The waits of a replace that never gets through: six, half a second.
const SCHEDULE: [u64; 6] = [10, 20, 40, 80, 150, 200];

/// Opens a file as a program does that wants it to itself: nobody else may
/// read, write or delete it while the handle is open.
fn hold_exclusively(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .expect("open the file with no sharing")
}

/// Opens a file as a viewer does: others may read it, and nobody may
/// replace it.
fn hold_for_reading(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .open(path)
        .expect("open the file sharing reads only")
}

fn seen(ws: &Workspace, path: &str, content: &[u8]) -> std::path::PathBuf {
    let file = ws.bytes(path, content);
    ok(&ws.call(&ReadTool::new(), json!({"path": path, "fresh": true})));
    file
}

fn edit_call(path: &str) -> serde_json::Value {
    json!({"path": path, "edits": [{"old": "original", "new": "replacement"}]})
}

#[test]
fn a_file_held_with_no_sharing_is_not_read_and_nothing_is_waited_for() {
    let ws = Workspace::new("win_edit_exclusive");
    let content = b"the original\r\n";
    let path = seen(&ws, "report.txt", content);
    let held = hold_exclusively(&path);
    for result in [
        ws.call(&EditTool::new(ws.editing()), edit_call("report.txt")),
        ws.call(
            &WriteTool::new(ws.editing()),
            json!({"path": "report.txt", "content": "all new\n"}),
        ),
    ] {
        assert_eq!(
            failed(&result),
            "`report.txt` is open in another program, which does not let it be read. Nothing \
             was changed. Close it there, or wait for that program to finish, and repeat the \
             call."
        );
    }
    assert!(
        ws.clock.sleeps().is_empty(),
        "a file that cannot be read is not waited for"
    );
    assert!(ws.checkpoints.changes().is_empty());
    drop(held);
    assert_eq!(ws.read_bytes("report.txt"), content);
    assert_eq!(ws.listing(), ["report.txt"]);

    // Let go, the same call goes through.
    ok(&ws.call(&EditTool::new(ws.editing()), edit_call("report.txt")));
    assert_eq!(ws.read_bytes("report.txt"), b"the replacement\r\n");
}

#[test]
fn a_file_that_stays_open_elsewhere_is_retried_then_left_exactly_as_it_was() {
    let ws = Workspace::new("win_edit_locked");
    let content = b"\xef\xbb\xbfthe original\r\nsecond line\r\n";
    let path = seen(&ws, "report.txt", content);
    let file = ws.paths.resolve("report.txt").unwrap().abs().clone();

    // It can be read, so the edit is worked out. It cannot be replaced.
    let held = hold_for_reading(&path);
    let message = failed(&ws.call(&EditTool::new(ws.editing()), edit_call("report.txt")));
    assert_eq!(
        message,
        "`report.txt` is open in another program, which kept it locked through 7 attempts over \
         500ms. Nothing was changed. Close it there, or wait for that program to finish, and \
         repeat the call."
    );
    assert_eq!(
        ws.clock.sleeps(),
        SCHEDULE.map(Duration::from_millis),
        "six retries, each after its wait, all on the fake clock"
    );

    // The failed call left the file byte for byte, no temporary file, and
    // the session still knowing the file as it is.
    assert_eq!(ws.read_bytes("report.txt"), content);
    assert_eq!(ws.listing(), ["report.txt"]);
    assert_eq!(
        ws.files.staleness(&file, &ContentHash::of(content)),
        Staleness::Current
    );
    // The round was told the file would change and has no result for it:
    // what it kept is what the file still holds.
    let changes = ws.checkpoints.changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].before.as_deref(), Some(&content[..]));
    assert_eq!(changes[0].after, None);

    // `write` meets the same lock, and leaves the same file.
    let message = failed(&ws.call(
        &WriteTool::new(ws.editing()),
        json!({"path": "report.txt", "content": "all new\n"}),
    ));
    assert!(
        message.contains("kept it locked through 7 attempts"),
        "{message}"
    );
    assert_eq!(ws.read_bytes("report.txt"), content);
    assert_eq!(ws.listing(), ["report.txt"]);

    // With the file let go, the edit goes through without another wait.
    drop(held);
    let waited = ws.clock.sleeps().len();
    ok(&ws.call(&EditTool::new(ws.editing()), edit_call("report.txt")));
    assert_eq!(
        ws.read_bytes("report.txt"),
        b"\xef\xbb\xbfthe replacement\r\nsecond line\r\n"
    );
    assert_eq!(ws.clock.sleeps().len(), waited);
}

/// A clock whose third wait is the moment the other program closes the
/// file.
#[derive(Debug)]
struct ReleasesOnThirdSleep {
    inner: FakeClock,
    held: Mutex<Option<File>>,
}

impl Clock for ReleasesOnThirdSleep {
    fn now(&self) -> UtcTime {
        self.inner.now()
    }

    fn monotonic(&self) -> Duration {
        self.inner.monotonic()
    }

    fn sleep(&self, duration: Duration) {
        self.inner.sleep(duration);
        if self.inner.sleeps().len() == 3 {
            self.held.lock().unwrap().take();
        }
    }
}

#[test]
fn a_file_let_go_during_the_retries_is_edited() {
    let ws = Workspace::new("win_edit_released");
    let path = seen(&ws, "report.txt", b"the original\r\n");
    let clock = Arc::new(ReleasesOnThirdSleep {
        inner: FakeClock::default(),
        held: Mutex::new(Some(hold_for_reading(&path))),
    });
    let editing = Editing::new(Arc::new(ws.checkpoints.clone()), clock.clone()).with_sync(false);
    let result = ws.call(&EditTool::new(editing), edit_call("report.txt"));
    assert_eq!(ok(&result), "Edited `report.txt`: line 1.");
    assert_eq!(ws.read_bytes("report.txt"), b"the replacement\r\n");
    assert_eq!(
        clock.inner.sleeps(),
        [10, 20, 40].map(Duration::from_millis),
        "it waited as long as the file was held, and no longer"
    );
    assert_eq!(ws.listing(), ["report.txt"]);
    let changes = ws.checkpoints.changes();
    assert_eq!(changes[0].before.as_deref(), Some(&b"the original\r\n"[..]));
    assert!(changes[0].after.is_some());
}

#[test]
fn a_file_with_the_read_only_attribute_is_not_replaced() {
    let ws = Workspace::new("win_edit_read_only");
    let path = seen(&ws, "locked.txt", b"the original\r\n");
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    let message = failed(&ws.call(&EditTool::new(ws.editing()), edit_call("locked.txt")));
    assert_eq!(
        message,
        "`locked.txt` is read-only, so it was not changed. Make it writable first if it is \
         meant to be changed."
    );
    assert_eq!(ws.read_bytes("locked.txt"), b"the original\r\n");
    assert_eq!(ws.listing(), ["locked.txt"]);
    assert!(ws.clock.sleeps().is_empty(), "a refusal is not waited on");
}

#[test]
fn a_file_read_under_one_spelling_is_edited_under_another() {
    let ws = Workspace::new("win_edit_spelling");
    seen(&ws, "Src/Main.rs", b"fn original() {}\r\n");
    // Another case, and the other separator: one file, which the session
    // has seen.
    let result = ws.call(&EditTool::new(ws.editing()), edit_call(r"src\MAIN.RS"));
    assert_eq!(ok(&result), "Edited `Src/Main.rs`: line 1.");
    assert_eq!(ws.read_bytes("Src/Main.rs"), b"fn replacement() {}\r\n");
    assert_eq!(
        ws.listing(),
        ["Src/Main.rs"],
        "the on-disk spelling is kept"
    );

    // A new file is written with the content as given: line feeds stay
    // line feeds on Windows too.
    ok(&ws.call(
        &WriteTool::new(ws.editing()),
        json!({"path": r"Src\new.rs", "content": "one\ntwo\n"}),
    ));
    assert_eq!(ws.read_bytes("Src/new.rs"), b"one\ntwo\n");
    // And the names Windows gives a second meaning are refused when the
    // call is planned.
    for path in [
        "NUL",
        "notes.txt.",
        "notes.txt::$DATA",
        r"\\server\share\x.txt",
    ] {
        let refused = ws.plan(
            &WriteTool::new(ws.editing()),
            json!({"path": path, "content": "x"}),
        );
        assert!(refused.is_err(), "`{path}` was planned");
    }
}
