//! Contract tests for `write` and `edit`: every row of their sections in
//! `docs/architecture/05-tools.md`, with "Text files", "Checkpoints" and
//! "Atomic writes", on real files.
//!
//! The matching itself is tested where it lives
//! (`crates/cortex-tools/src/matching/tests.rs`). Here the tools are run as
//! the loop runs them, and what is checked is what reaches the disk, the
//! table of seen files, the checkpoints and the model.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cortex_base::{AbsPath, CancelToken, ContentHash};
use cortex_protocol::item::ToolOutcome;
use cortex_testkit::cases::{self, ensure};
use cortex_testkit::{Rng, SeededRng};
use cortex_tools::{
    CheckpointError, Checkpoints, Concurrency, ConcurrencyClass, EDIT_LIMIT, EditTool, Editing,
    Effect, FileSnapshot, ReadTool, Staleness, Tool, WriteTool,
};
use serde_json::{Value, json};

use crate::{Workspace, failed, ok};

fn write(ws: &Workspace) -> WriteTool {
    WriteTool::new(ws.editing())
}

fn edit(ws: &Workspace) -> EditTool {
    EditTool::new(ws.editing())
}

/// Reads a file with the `read` tool, so that the session has seen it.
fn read(ws: &Workspace, path: &str) {
    ok(&ws.call(&ReadTool::new(), json!({"path": path, "fresh": true})));
}

/// A file that the session has read.
fn seen_file(ws: &Workspace, path: &str, content: &[u8]) {
    ws.bytes(path, content);
    read(ws, path);
}

fn one_edit(path: &str, old: &str, new: &str) -> Value {
    json!({"path": path, "edits": [{"old": old, "new": new}]})
}

fn abs(ws: &Workspace, path: &str) -> AbsPath {
    ws.paths.resolve(path).unwrap().abs().clone()
}

// --- write --------------------------------------------------------------------

#[test]
fn write_creates_a_file_and_its_directories_with_the_content_as_given() {
    let ws = Workspace::new("write_create");
    // Exactly as given: CRLF, and no line break at the end.
    let content = "first\r\nsecond\nlast";
    let result = ws.call(
        &write(&ws),
        json!({"path": "src/deep/new.txt", "content": content}),
    );
    assert_eq!(ok(&result), "Created `src/deep/new.txt`: 3 lines.");
    assert_eq!(ws.read_bytes("src/deep/new.txt"), content.as_bytes());
    assert_eq!(ws.listing(), ["src/deep/new.txt"], "no temporary file");

    let payload = result.payload().expect("a payload for front ends");
    assert_eq!(payload["kind"], "create");
    assert_eq!(payload["path"], "src/deep/new.txt");
    assert_eq!(payload["lines"], 3);
    assert_eq!(payload["added"], 3);
    assert_eq!(payload["diff"], "@@ -0,0 +1,3 @@\n+first\n+second\n+last");

    // The checkpoint knows the file did not exist, and what was left.
    let file = abs(&ws, "src/deep/new.txt");
    let changes = ws.checkpoints.changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].file, file);
    assert_eq!(changes[0].before, None);
    let left = changes[0].after.as_ref().expect("what the write left");
    assert_eq!(left.hash, ContentHash::of(content.as_bytes()));
    // And the session has seen what it wrote: it may change it again.
    assert_eq!(
        ws.files
            .staleness(&file, &ContentHash::of(content.as_bytes())),
        Staleness::Current
    );
}

#[test]
fn write_declares_one_write_keyed_by_the_real_path() {
    let ws = Workspace::new("write_plan");
    let tool = write(&ws);
    assert_eq!(tool.spec().concurrency, ConcurrencyClass::Keyed);
    let plan = ws
        .plan(
            &tool,
            json!({"path": "./src/../src/a.rs", "content": "x\ny\n"}),
        )
        .unwrap();
    let file = ws.paths.resolve("src/a.rs").unwrap();
    assert_eq!(plan.effects, [Effect::Write(file.clone())]);
    assert_eq!(
        plan.concurrency,
        Concurrency::Keyed(vec![file.abs().clone()])
    );
    assert_eq!(plan.summary, "Write src/a.rs (2 lines)");
    assert!(plan.has_side_effects());
    assert!(!plan.changes_session());
    // Planning touched nothing.
    assert!(ws.listing().is_empty());

    // A read of the same file waits for it, and a read of another does not.
    let read_tool = ReadTool::new();
    let same = ws.plan(&read_tool, json!({"path": "src/a.rs"})).unwrap();
    let other = ws.plan(&read_tool, json!({"path": "src/b.rs"})).unwrap();
    assert!(plan.conflicts_with(&same));
    assert!(!plan.conflicts_with(&other));
    let edit_plan = ws.plan(&edit(&ws), one_edit("src/a.rs", "x", "z")).unwrap();
    assert!(plan.conflicts_with(&edit_plan), "one file, one at a time");
    assert_eq!(edit_plan.effects, [Effect::Write(file)]);
    assert_eq!(edit_plan.summary, "Edit src/a.rs (1 edit)");
}

#[test]
fn the_arguments_are_checked_when_the_call_is_planned() {
    let ws = Workspace::new("write_arguments");
    ws.dir("src");
    let refused = |tool: &dyn Tool, args: Value| {
        ws.plan(tool, args)
            .expect_err("the call must be refused")
            .to_string()
    };
    let w = write(&ws);
    let e = edit(&ws);
    assert!(refused(&w, json!({"path": "a.txt"})).contains("content"));
    assert!(refused(&w, json!({"path": "", "content": "x"})).contains("path"));
    assert!(refused(&w, json!({"path": "a", "content": "x", "mode": "a"})).contains("mode"));
    assert!(
        refused(&w, json!({"path": "out/", "content": "x"})).contains("names a directory"),
        "a path that ends with a separator is a directory"
    );
    assert!(refused(&e, json!({"path": "a.txt", "edits": []})).contains("edits"));
    assert!(refused(&e, one_edit("a.txt", "", "x")).contains("old"));
    assert_eq!(
        refused(&e, one_edit("a.txt", "same", "same")),
        "The edit: `old` and `new` are the same text, so there is nothing to change."
    );
    assert!(
        refused(
            &e,
            json!({"path": "a.txt", "edits": [
                {"old": "a", "new": "b"},
                {"old": "c", "new": "c"}
            ]})
        )
        .starts_with("Edit 2:")
    );
    assert!(
        refused(
            &e,
            json!({"path": "a.txt", "edits": [{"old": "a", "new": "b", "count": 0}]})
        )
        .contains("count")
    );
    // A count is at most ten thousand: with a hundred edits to a call,
    // that bounds what one call can be asked to replace.
    for count in [10_001_u64, u64::MAX] {
        assert!(
            refused(
                &e,
                json!({"path": "a.txt", "edits": [{"old": "a", "new": "b", "count": count}]})
            )
            .contains("count"),
            "a count of {count}"
        );
    }
    // Nothing was touched by any of it.
    assert!(ws.listing().is_empty());
    // A directory that exists is refused when the call runs.
    assert_eq!(
        failed(&ws.call(&w, json!({"path": "src", "content": "x"}))),
        "`src` is a directory, not a file."
    );
}

#[test]
fn a_file_that_was_not_read_is_not_overwritten() {
    let ws = Workspace::new("write_unseen");
    ws.file("notes.md", "the user's own words\n");
    let message = failed(&ws.call(
        &write(&ws),
        json!({"path": "notes.md", "content": "replaced\n"}),
    ));
    assert_eq!(
        message,
        "`notes.md` exists and has not been read in this session. Read it first, then \
         overwrite it: this keeps a file you have not seen from being changed. Nothing was \
         changed."
    );
    assert_eq!(ws.read_bytes("notes.md"), b"the user's own words\n");
    assert!(ws.checkpoints.changes().is_empty());

    let message = failed(&ws.call(&edit(&ws), one_edit("notes.md", "own", "very own")));
    assert!(
        message.contains("has not been read in this session"),
        "{message}"
    );
    assert!(message.contains("then edit it"), "{message}");
    assert_eq!(ws.read_bytes("notes.md"), b"the user's own words\n");
}

#[test]
fn a_file_that_changed_since_it_was_read_is_not_overwritten() {
    let ws = Workspace::new("write_stale");
    seen_file(&ws, "notes.md", b"as the model read it\n");
    // Someone edits the file.
    ws.file("notes.md", "as the user left it\n");
    for result in [
        ws.call(
            &write(&ws),
            json!({"path": "notes.md", "content": "replaced\n"}),
        ),
        ws.call(&edit(&ws), one_edit("notes.md", "left", "LEFT")),
    ] {
        let message = failed(&result);
        assert!(
            message.starts_with(
                "`notes.md` has changed on disk since it was last read or written in this \
                 session. Read it again"
            ),
            "{message}"
        );
        assert_eq!(ws.read_bytes("notes.md"), b"as the user left it\n");
    }
    assert!(ws.checkpoints.changes().is_empty());

    // Read again, and the change goes through.
    read(&ws, "notes.md");
    ok(&ws.call(&edit(&ws), one_edit("notes.md", "left", "LEFT")));
    assert_eq!(ws.read_bytes("notes.md"), b"as the user LEFT it\n");
}

#[test]
fn write_replaces_a_file_that_was_read_and_keeps_what_it_was() {
    let ws = Workspace::new("write_replace");
    seen_file(&ws, "a.txt", b"one\ntwo\nthree\n");
    let result = ws.call(
        &write(&ws),
        json!({"path": "a.txt", "content": "one\n2\nthree\nfour\n"}),
    );
    assert_eq!(ok(&result), "Wrote `a.txt`: 4 lines (2 added, 1 removed).");
    assert_eq!(ws.read_bytes("a.txt"), b"one\n2\nthree\nfour\n");
    let payload = result.payload().unwrap();
    assert_eq!(payload["kind"], "write");
    assert_eq!(
        payload["diff"],
        "@@ -1,3 +1,4 @@\n one\n-two\n+2\n three\n+four"
    );
    let changes = ws.checkpoints.changes();
    assert_eq!(
        changes[0].before.as_deref(),
        Some(&b"one\ntwo\nthree\n"[..])
    );

    // What the tool wrote is what the session has seen: a second change
    // needs no read in between, and the round still knows the first state.
    ok(&ws.call(&edit(&ws), one_edit("a.txt", "four", "4")));
    let changes = ws.checkpoints.changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].before.as_deref(),
        Some(&b"one\ntwo\nthree\n"[..])
    );
    assert_eq!(
        changes[0].after.as_ref().unwrap().hash,
        ContentHash::of(b"one\n2\nthree\n4\n")
    );
    // And a read after a write is of the new content, in full.
    let shown = ok(&ws.call(&ReadTool::new(), json!({"path": "a.txt"})));
    assert_eq!(shown, "a.txt (4 lines)\none\n2\nthree\n4");

    // The same content again writes nothing.
    ws.checkpoints.clear();
    let result = ws.call(
        &write(&ws),
        json!({"path": "a.txt", "content": "one\n2\nthree\n4\n"}),
    );
    assert_eq!(
        ok(&result),
        "`a.txt` already has exactly this content. Nothing was written."
    );
    assert!(ws.checkpoints.changes().is_empty());
}

#[test]
fn write_keeps_the_line_endings_and_the_mark_of_the_file_it_replaces() {
    let ws = Workspace::new("write_endings");
    seen_file(&ws, "crlf.txt", b"\xef\xbb\xbfone\r\ntwo\r\n");
    ok(&ws.call(
        &write(&ws),
        json!({"path": "crlf.txt", "content": "one\ntwo\nthree\n"}),
    ));
    assert_eq!(
        ws.read_bytes("crlf.txt"),
        b"\xef\xbb\xbfone\r\ntwo\r\nthree\r\n",
        "the mark and CRLF stay"
    );
    // A mark in the new content is not written twice.
    ok(&ws.call(
        &write(&ws),
        json!({"path": "crlf.txt", "content": "\u{feff}only\n"}),
    ));
    assert_eq!(ws.read_bytes("crlf.txt"), b"\xef\xbb\xbfonly\r\n");

    // Both kinds of ending: the lines that stay keep theirs.
    seen_file(
        &ws,
        "mixed.txt",
        b"keep a\r\nold\nkeep b\nkeep c\r\nend\r\n",
    );
    ok(&ws.call(
        &write(&ws),
        json!({"path": "mixed.txt", "content": "keep a\nnew\nkeep b\nkeep c\nend\n"}),
    ));
    assert_eq!(
        ws.read_bytes("mixed.txt"),
        b"keep a\r\nnew\r\nkeep b\nkeep c\r\nend\r\n"
    );
}

// --- edit ---------------------------------------------------------------------

#[test]
fn edit_replaces_the_text_named_and_says_where() {
    let ws = Workspace::new("edit_basic");
    seen_file(
        &ws,
        "src/main.rs",
        b"fn main() {\n    println!(\"hi\");\n}\n\nfn other() {}\n",
    );
    let result = ws.call(
        &edit(&ws),
        json!({"path": "src/main.rs", "edits": [
            {"old": "println!(\"hi\");", "new": "println!(\"hello\");\n    done();"},
            {"old": "fn other() {}\n", "new": ""}
        ]}),
    );
    assert_eq!(
        ok(&result),
        "Edited `src/main.rs`: lines 2-3 and text removed at line 6."
    );
    assert_eq!(
        ws.read_bytes("src/main.rs"),
        b"fn main() {\n    println!(\"hello\");\n    done();\n}\n\n"
    );
    let payload = result.payload().unwrap();
    assert_eq!(payload["kind"], "edit");
    assert_eq!(payload["tolerant"], false);
    assert_eq!(payload["replacements"], 2);
    assert_eq!(payload["ranges"], json!([[2, 3], [6, 6]]));
    assert_eq!(
        (&payload["added"], &payload["removed"]),
        (&json!(2), &json!(2))
    );
    let diff = payload["diff"].as_str().unwrap();
    assert!(diff.contains("-    println!(\"hi\");\n+    println!(\"hello\");\n+    done();"));
    // The model is not sent the diff: it knows what it asked for.
    assert!(!ok(&result).contains("@@"));
    assert_eq!(ws.listing(), ["src/main.rs"], "no temporary file");
}

#[test]
fn edit_of_a_file_that_is_not_there_says_to_create_it() {
    let ws = Workspace::new("edit_missing");
    assert_eq!(
        failed(&ws.call(&edit(&ws), one_edit("gone.rs", "a", "b"))),
        "`gone.rs` does not exist, so there is nothing to edit. Use `write` to create it."
    );
    assert!(ws.listing().is_empty());
}

#[test]
fn every_occurrence_counted_is_replaced_and_any_other_number_is_refused() {
    let ws = Workspace::new("edit_count");
    let content = b"let a = 1;\nlet b = 1;\nlet c = 1;\n";
    seen_file(&ws, "v.rs", content);
    let message = failed(&ws.call(&edit(&ws), one_edit("v.rs", " = 1;", " = 2;")));
    assert_eq!(
        message,
        "`edit` changed nothing in `v.rs`.\nThe edit: `old` occurs 3 times (at lines 1, 2 and \
         3), and `count` is 1. Add lines around it to `old` until it names the place you mean, \
         or set `count` to 3 to replace every one."
    );
    assert_eq!(ws.read_bytes("v.rs"), content);
    let result = ws.call(
        &edit(&ws),
        json!({"path": "v.rs", "edits": [{"old": " = 1;", "new": " = 2;", "count": 3}]}),
    );
    assert_eq!(ok(&result), "Edited `v.rs`: line 1, line 2 and line 3.");
    assert_eq!(
        ws.read_bytes("v.rs"),
        b"let a = 2;\nlet b = 2;\nlet c = 2;\n"
    );
}

#[test]
fn either_every_edit_applies_or_none_does() {
    let ws = Workspace::new("edit_all_or_none");
    let content = b"alpha\nbeta\ngamma\n";
    seen_file(&ws, "greek.txt", content);
    let message = failed(&ws.call(
        &edit(&ws),
        json!({"path": "greek.txt", "edits": [
            {"old": "alpha", "new": "A"},
            {"old": "delta", "new": "D"},
            {"old": "alpha\nbeta", "new": "AB"}
        ]}),
    ));
    assert!(
        message.starts_with("`edit` changed nothing in `greek.txt`."),
        "{message}"
    );
    assert!(
        message.contains("Edits 1 and 3 would replace the same text (line 1 and lines 1-2)"),
        "{message}"
    );
    assert!(
        message.contains("Edit 2: `old` was not found."),
        "{message}"
    );
    assert_eq!(ws.read_bytes("greek.txt"), content);
    assert!(ws.checkpoints.changes().is_empty());
    assert_eq!(ws.listing(), ["greek.txt"]);
}

#[test]
fn a_failed_edit_shows_the_closest_text_and_how_it_differs() {
    let ws = Workspace::new("edit_closest");
    let content = b"fn area(w: u32, h: u32) -> u32 {\n\tw * h\n}\n";
    seen_file(&ws, "geo.rs", content);
    let message = failed(&ws.call(
        &edit(&ws),
        one_edit(
            "geo.rs",
            "fn area(w: u32, h: u32) -> u64 {\n\tw * h\n}",
            "x",
        ),
    ));
    assert_eq!(
        message,
        "`edit` changed nothing in `geo.rs`.\nThe edit: `old` was not found. The closest text \
         is at lines 1-3. Lines marked `-` are in your `old`, and lines marked `+` are what the \
         file has there:\n@@ -1,3 +1,3 @@\n-fn area(w: u32, h: u32) -> u64 {\n+fn area(w: u32, \
         h: u32) -> u32 {\n \tw * h\n }"
    );
    // Spaces where the file has a tab: the diff alone would not show why.
    let message = failed(&ws.call(
        &edit(&ws),
        one_edit(
            "geo.rs",
            "fn area(w: u32, h: u32) -> u32 {\n    w * h\n}",
            "x",
        ),
    ));
    assert!(
        message.ends_with(
            "Line 2 of the file is indented with 1 tab, and the matching line of `old` with 4 \
             spaces."
        ),
        "{message}"
    );
    let message = failed(&ws.call(&edit(&ws), one_edit("geo.rs", "nothing like it", "x")));
    assert!(
        message.contains("Nothing in the file is close to it."),
        "{message}"
    );
    assert_eq!(ws.read_bytes("geo.rs"), content);
}

#[test]
fn the_tolerant_tier_is_reported_with_the_text_it_replaced() {
    let ws = Workspace::new("edit_tolerant");
    seen_file(
        &ws,
        "lib.rs",
        b"impl A {\n    fn f(&self) {   \n        work();\n    }\n}\n",
    );
    // No indentation, and no trailing spaces: not what the file has.
    let result = ws.call(
        &edit(&ws),
        one_edit(
            "lib.rs",
            "fn f(&self) {\n    work();\n}",
            "fn f(&self) {\n    work();\n    more();\n}",
        ),
    );
    assert_eq!(
        ok(&result),
        "Edited `lib.rs`: lines 2-5.\nThe edit did not match exactly. A match that ignores \
         trailing white space and a uniform difference in indentation was used, at lines 2-4. \
         The text replaced was:\n    fn f(&self) {   \n        work();\n    }\nThe new text \
         was written with 4 spaces added to each of its lines, to match."
    );
    assert_eq!(result.payload().unwrap()["tolerant"], true);
    assert_eq!(
        ws.read_bytes("lib.rs"),
        b"impl A {\n    fn f(&self) {\n        work();\n        more();\n    }\n}\n"
    );

    // An exact match says nothing of the kind.
    let result = ws.call(&edit(&ws), one_edit("lib.rs", "more();", "even_more();"));
    assert_eq!(ok(&result), "Edited `lib.rs`: line 4.");
    assert_eq!(result.payload().unwrap()["tolerant"], false);

    // The tolerant tier must find exactly `count` places too.
    seen_file(&ws, "two.rs", b"  go();\n    go();\n");
    let message = failed(&ws.call(&edit(&ws), one_edit("two.rs", "go(); ", "stop();")));
    assert!(
        message.contains(
            "`old` is not in the file exactly. With trailing white space and indentation \
             ignored it matches twice (at lines 1 and 2), and `count` is 1."
        ),
        "{message}"
    );
}

#[test]
fn line_endings_and_a_mark_survive_an_edit_byte_for_byte() {
    let ws = Workspace::new("edit_endings");
    // A mark, CRLF, and no line break at the end.
    seen_file(&ws, "win.txt", b"\xef\xbb\xbfone\r\ntwo\r\nthree");
    // `old` as `read` shows it: line feeds, no mark.
    let result = ws.call(&edit(&ws), one_edit("win.txt", "one\ntwo", "1\n2\n2b"));
    assert_eq!(ok(&result), "Edited `win.txt`: lines 1-3.");
    assert_eq!(
        result.payload().unwrap()["tolerant"],
        false,
        "an exact match"
    );
    assert_eq!(
        ws.read_bytes("win.txt"),
        b"\xef\xbb\xbf1\r\n2\r\n2b\r\nthree"
    );

    // Both kinds of ending in one file.
    seen_file(&ws, "mixed.txt", b"a\r\nb\nc\r\nd\n");
    ok(&ws.call(&edit(&ws), one_edit("mixed.txt", "b", "B\nB2")));
    assert_eq!(ws.read_bytes("mixed.txt"), b"a\r\nB\nB2\nc\r\nd\n");
    ok(&ws.call(&edit(&ws), one_edit("mixed.txt", "c", "C\nC2")));
    assert_eq!(ws.read_bytes("mixed.txt"), b"a\r\nB\nB2\nC\r\nC2\r\nd\n");

    // An edit that only rewrites how a line break is written changes
    // nothing, and says so.
    ws.checkpoints.clear();
    let result = ws.call(&edit(&ws), one_edit("mixed.txt", "a\nB", "a\r\nB"));
    assert!(ok(&result).contains("Nothing was written."));
    assert!(ws.checkpoints.changes().is_empty());
}

// --- Text files ---------------------------------------------------------------

#[test]
fn a_file_that_is_not_utf8_is_refused_and_never_written_back() {
    let ws = Workspace::new("edit_not_utf8");
    // Latin-1: `café`, with the é as one byte.
    let content = b"name = caf\xe9\nother = 1\n";
    ws.bytes("latin1.txt", content);
    // `read` shows it, with the byte replaced, and the session has seen it.
    let shown = ok(&ws.call(&ReadTool::new(), json!({"path": "latin1.txt"})));
    assert!(shown.contains("caf\u{fffd}"), "{shown}");

    // An edit that does not even touch the bad line.
    let message = failed(&ws.call(&edit(&ws), one_edit("latin1.txt", "other = 1", "other = 2")));
    assert_eq!(
        message,
        "`latin1.txt` is not valid UTF-8: the first byte that is not is on line 1. `read` \
         showed it with such bytes replaced, and writing that back would damage the file, so \
         the editing tools refuse it. Nothing was changed. To change it, use a shell command \
         that works on bytes."
    );
    // And a write of what `read` showed.
    let message = failed(&ws.call(
        &write(&ws),
        json!({"path": "latin1.txt", "content": "name = caf\u{fffd}\nother = 2\n"}),
    ));
    assert!(message.contains("is not valid UTF-8"), "{message}");
    assert_eq!(ws.read_bytes("latin1.txt"), content, "not a byte changed");
    assert!(ws.checkpoints.changes().is_empty());
}

#[test]
fn a_binary_file_is_refused() {
    let ws = Workspace::new("edit_binary");
    let content = b"\x7fELF\x02\x01\x01\0rest of a program";
    ws.bytes("tool", content);
    for result in [
        ws.call(&write(&ws), json!({"path": "tool", "content": "text\n"})),
        ws.call(&edit(&ws), one_edit("tool", "rest", "REST")),
    ] {
        let message = failed(&result);
        assert!(
            message.starts_with("`tool` is not a text file: it is an ELF executable"),
            "{message}"
        );
    }
    assert_eq!(ws.read_bytes("tool"), content);
}

#[test]
fn a_file_too_large_to_keep_for_undo_is_refused_with_another_way() {
    let ws = Workspace::new("edit_too_large");
    assert_eq!(EDIT_LIMIT, 16 * 1024 * 1024, "what a checkpoint keeps");
    let content = "line\n".repeat(300);
    seen_file(&ws, "big.txt", content.as_bytes());
    let small = Editing::with_limit(ws.editing(), 1024);
    for result in [
        ws.call(
            &WriteTool::new(small.clone()),
            json!({"path": "big.txt", "content": "x\n"}),
        ),
        ws.call(
            &EditTool::new(small.clone()),
            one_edit("big.txt", "line", "LINE"),
        ),
    ] {
        assert_eq!(
            failed(&result),
            "`big.txt` is 1.5 KiB, and the editing tools change files of up to 1.0 KiB: what \
             a larger file held could not be kept, so the change could not be undone. Nothing \
             was changed. To change it all the same, use a shell command (for example `sed`, \
             or a short script)."
        );
    }
    assert_eq!(ws.read_bytes("big.txt"), content.as_bytes());
    // At the limit exactly, the file is changed.
    let fits = "ab".repeat(512);
    seen_file(&ws, "fits.txt", fits.as_bytes());
    ok(&ws.call(
        &EditTool::new(small),
        json!({"path": "fits.txt", "edits": [{"old": "ab", "new": "cd", "count": 512}]}),
    ));
    assert_eq!(ws.read_bytes("fits.txt"), "cd".repeat(512).as_bytes());
}

// --- Checkpoints, cancellation and failure ------------------------------------

/// A checkpoint store that cannot keep anything.
struct BrokenCheckpoints;

impl Checkpoints for BrokenCheckpoints {
    fn before(&self, _file: &AbsPath, _content: Option<&[u8]>) -> Result<(), CheckpointError> {
        Err(CheckpointError::new("the session's disk is full"))
    }

    fn after(&self, _file: &AbsPath, _left: Option<&FileSnapshot>) {
        panic!("nothing was kept, so nothing was written, and there is nothing to report");
    }
}

#[test]
fn a_file_whose_earlier_content_cannot_be_kept_is_not_changed() {
    let ws = Workspace::new("edit_no_checkpoint");
    seen_file(&ws, "a.txt", b"as it was\n");
    let broken = Editing::new(Arc::new(BrokenCheckpoints), Arc::new(ws.clock.clone()));
    for result in [
        ws.call(
            &EditTool::new(broken.clone()),
            one_edit("a.txt", "was", "is"),
        ),
        ws.call(
            &WriteTool::new(broken.clone()),
            json!({"path": "a.txt", "content": "new\n"}),
        ),
        ws.call(
            &WriteTool::new(broken.clone()),
            json!({"path": "new.txt", "content": "new\n"}),
        ),
        // Not even the directories of a file that is not written are made.
        ws.call(
            &WriteTool::new(broken.clone()),
            json!({"path": "made/for/nothing.txt", "content": "new\n"}),
        ),
    ] {
        let message = failed(&result);
        assert!(
            message.contains(
                "could not be kept for undo (the session's disk is full), so it was not changed"
            ),
            "{message}"
        );
    }
    assert_eq!(ws.read_bytes("a.txt"), b"as it was\n");
    assert_eq!(ws.listing(), ["a.txt"]);
    assert!(!ws.root().join("made").exists(), "no directory was left");
}

#[test]
fn files_at_the_edges_of_what_text_is_are_changed_exactly() {
    let ws = Workspace::new("edit_edges");
    // An empty file: nothing to match, and nothing to keep.
    seen_file(&ws, "empty.txt", b"");
    let message = failed(&ws.call(&edit(&ws), one_edit("empty.txt", "x", "y")));
    assert!(message.contains("`old` was not found."), "{message}");
    assert_eq!(ws.read_bytes("empty.txt"), b"");
    ok(&ws.call(
        &write(&ws),
        json!({"path": "empty.txt", "content": "now\r\nwith text"}),
    ));
    assert_eq!(
        ws.read_bytes("empty.txt"),
        b"now\r\nwith text",
        "there was no line ending to keep, so the content is as given"
    );

    // A carriage return that is not part of a line break is content.
    seen_file(&ws, "cr.txt", b"one\rstill one\ntwo\n");
    ok(&ws.call(&edit(&ws), one_edit("cr.txt", "two", "2")));
    assert_eq!(ws.read_bytes("cr.txt"), b"one\rstill one\n2\n");
    ok(&ws.call(
        &edit(&ws),
        one_edit("cr.txt", "one\rstill", "one\rstill and"),
    ));
    assert_eq!(ws.read_bytes("cr.txt"), b"one\rstill and one\n2\n");
    // And one at the very end of what `write` is given stays where it is.
    ok(&ws.call(&write(&ws), json!({"path": "cr.txt", "content": "a\nb\r"})));
    assert_eq!(ws.read_bytes("cr.txt"), b"a\nb\r");

    // One line with no line break, replaced whole and in part.
    seen_file(&ws, "one.txt", "naïve café".as_bytes());
    ok(&ws.call(&edit(&ws), one_edit("one.txt", "ï", "i")));
    assert_eq!(ws.read_bytes("one.txt"), "naive café".as_bytes());
    ok(&ws.call(
        &edit(&ws),
        one_edit("one.txt", "naive café", "日本語\nline two"),
    ));
    assert_eq!(ws.read_bytes("one.txt"), "日本語\nline two".as_bytes());

    // Edits that leave the file as it is write nothing, and say so.
    seen_file(&ws, "same.txt", b"  keep\n");
    ws.checkpoints.clear();
    let result = ws.call(&edit(&ws), one_edit("same.txt", "keep ", "keep"));
    assert_eq!(
        ok(&result),
        "`same.txt` already has this text: with the edits made it would be exactly as it is. \
         Nothing was written."
    );
    assert!(ws.checkpoints.changes().is_empty());
}

#[test]
fn a_call_cancelled_before_it_starts_changes_nothing() {
    let ws = Workspace::new("edit_cancelled");
    seen_file(&ws, "a.txt", b"as it was\n");
    let cancel = CancelToken::new();
    cancel.cancel();
    let cx = ws.cx().with_cancel(cancel);
    for result in [
        ws.call_with(&cx, &edit(&ws), one_edit("a.txt", "was", "is")),
        ws.call_with(&cx, &write(&ws), json!({"path": "b.txt", "content": "x"})),
    ] {
        assert_eq!(result.outcome(), &ToolOutcome::Interrupted);
        assert!(result.text().contains("Nothing was changed."));
    }
    assert_eq!(ws.read_bytes("a.txt"), b"as it was\n");
    assert_eq!(ws.listing(), ["a.txt"]);
    assert!(ws.checkpoints.changes().is_empty());
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::skipped;

    #[test]
    fn a_read_only_file_is_not_replaced() {
        let ws = Workspace::new("edit_read_only");
        let path = ws.bytes("locked.txt", b"protected\n");
        read(&ws, "locked.txt");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        if fs::OpenOptions::new().write(true).open(&path).is_ok() {
            skipped("running as a user that file permissions do not stop");
            return;
        }
        let message = failed(&ws.call(&edit(&ws), one_edit("locked.txt", "protected", "open")));
        assert_eq!(
            message,
            "`locked.txt` is read-only, so it was not changed. Make it writable first if it is \
             meant to be changed."
        );
        assert_eq!(ws.read_bytes("locked.txt"), b"protected\n");
        assert_eq!(ws.listing(), ["locked.txt"], "no temporary file is left");
        // The round was told the file would change, and then that it was
        // left as it was: nothing changed, so nothing is listed.
        assert!(ws.checkpoints.changes().is_empty());
    }

    #[test]
    fn the_mode_of_an_edited_file_is_kept() {
        let ws = Workspace::new("edit_mode");
        let path = ws.bytes("run.sh", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
        read(&ws, "run.sh");
        ok(&ws.call(&edit(&ws), one_edit("run.sh", "hi", "hello")));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o750);
    }

    #[test]
    fn a_path_through_a_link_names_and_changes_the_file_it_leads_to() {
        let ws = Workspace::new("edit_through_link");
        ws.bytes("real/config.toml", b"level = 1\n");
        symlink("real/config.toml", ws.root().join("config.toml")).unwrap();
        read(&ws, "config.toml");
        // The effect is on the real file: that is what a rule is matched
        // against, and what is checkpointed.
        let plan = ws
            .plan(&edit(&ws), one_edit("config.toml", "1", "2"))
            .unwrap();
        let real = ws.paths.resolve("real/config.toml").unwrap();
        assert_eq!(plan.effects, [Effect::Write(real.clone())]);
        let result = ws.call(&edit(&ws), one_edit("config.toml", "1", "2"));
        assert_eq!(ok(&result), "Edited `real/config.toml`: line 1.");
        assert_eq!(ws.read_bytes("real/config.toml"), b"level = 2\n");
        assert!(
            fs::symlink_metadata(ws.root().join("config.toml"))
                .unwrap()
                .is_symlink(),
            "the link is still a link"
        );
        assert_eq!(ws.checkpoints.changes()[0].file, *real.abs());

        // A link that leaves the workspace: the effect says so, for the
        // permission engine to decide.
        let outside = ws.dir.join("outside.txt");
        fs::write(&outside, "elsewhere\n").unwrap();
        symlink(&outside, ws.root().join("out.txt")).unwrap();
        let plan = ws
            .plan(&write(&ws), json!({"path": "out.txt", "content": "x"}))
            .unwrap();
        let Effect::Write(target) = &plan.effects[0] else {
            panic!("a write effect");
        };
        assert!(!target.is_inside_workspace());
    }

    /// Between the check of a call and its run there can be a long wait
    /// (an approval). A file that is swapped for a link in that time must
    /// not carry the write out of the workspace.
    #[test]
    fn a_file_swapped_for_a_link_after_the_call_was_planned_is_not_written_through() {
        let ws = Workspace::new("edit_swap_link");
        let outside = ws.dir.join("outside.txt");
        fs::write(&outside, "the user's file elsewhere\n").unwrap();
        seen_file(&ws, "notes.txt", b"harmless\n");
        ws.dir("sub");
        seen_file(&ws, "sub/inner.txt", b"harmless\n");
        let cx = ws.cx();
        let (w, e) = (write(&ws), edit(&ws));

        // The file itself becomes a link.
        let planned_write = w
            .plan(
                &json!({"path": "notes.txt", "content": "overwritten\n"}),
                &cx.plan_cx(),
            )
            .unwrap();
        let planned_edit = e
            .plan(&one_edit("notes.txt", "harmless", "changed"), &cx.plan_cx())
            .unwrap();
        // And a directory on the way to another.
        let planned_inner = e
            .plan(
                &one_edit("sub/inner.txt", "harmless", "changed"),
                &cx.plan_cx(),
            )
            .unwrap();
        let planned_new = w
            .plan(
                &json!({"path": "sub/new.txt", "content": "new\n"}),
                &cx.plan_cx(),
            )
            .unwrap();

        fs::remove_file(ws.root().join("notes.txt")).unwrap();
        symlink(&outside, ws.root().join("notes.txt")).unwrap();
        fs::write(ws.dir.join("inner.txt"), "the user's file elsewhere\n").unwrap();
        fs::remove_dir_all(ws.root().join("sub")).unwrap();
        symlink(&ws.dir, ws.root().join("sub")).unwrap();

        for (tool, plan) in [
            (&w as &dyn Tool, planned_write),
            (&e, planned_edit),
            (&e, planned_inner),
            (&w, planned_new),
        ] {
            let result = crate::block_on(cortex_tools::execute(
                tool,
                plan,
                &cx,
                &mut cortex_tools::NoOutput,
            ));
            let message = failed(&result);
            assert!(
                message.contains("no longer leads where it did")
                    && message.contains("Nothing was changed."),
                "{message}"
            );
        }
        assert_eq!(
            fs::read(&outside).unwrap(),
            b"the user's file elsewhere\n",
            "the file outside the workspace is untouched"
        );
        assert_eq!(
            fs::read(ws.dir.join("inner.txt")).unwrap(),
            b"the user's file elsewhere\n"
        );
        assert!(
            !ws.dir.join("new.txt").exists(),
            "nothing was created outside"
        );
        assert!(ws.checkpoints.changes().is_empty());
    }

    /// A file swapped for a FIFO: opening one waits for a writer that may
    /// never come. `mkfifo` is a process, hence the `sys_` name.
    #[test]
    fn sys_a_file_swapped_for_a_fifo_is_refused_without_waiting() {
        let ws = Workspace::new("sys_edit_swap_fifo");
        seen_file(&ws, "build.log", b"harmless\n");
        let cx = ws.cx();
        let (w, e) = (write(&ws), edit(&ws));
        let planned_edit = e
            .plan(&one_edit("build.log", "harmless", "changed"), &cx.plan_cx())
            .unwrap();
        let planned_write = w
            .plan(
                &json!({"path": "build.log", "content": "x\n"}),
                &cx.plan_cx(),
            )
            .unwrap();

        let path = ws.root().join("build.log");
        fs::remove_file(&path).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo failed");

        // The calls run on a thread of their own, so that a call that
        // waits on the FIFO is a failed test and not a hung one.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (tool, plan) in [(&e as &dyn Tool, planned_edit), (&w, planned_write)] {
                let result = crate::block_on(cortex_tools::execute(
                    tool,
                    plan,
                    &cx,
                    &mut cortex_tools::NoOutput,
                ));
                let _ = done_tx.send(result);
            }
        });
        for _ in 0..2 {
            let result = done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("the call waited on a FIFO nobody writes to");
            assert_eq!(
                failed(&result),
                "`build.log` is not a regular file (it is a device, a pipe or a socket), so it \
                 is not changed."
            );
        }
    }
}

// --- Properties -----------------------------------------------------------------

/// A generated file: lines that each hold a word no other line has, with
/// the endings and the mark the case asks for.
#[derive(Debug, Clone)]
struct Sample {
    lines: Vec<(String, &'static str)>,
    mark: bool,
}

impl Sample {
    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        if self.mark {
            bytes.extend_from_slice("\u{feff}".as_bytes());
        }
        for (content, ending) in &self.lines {
            bytes.extend_from_slice(content.as_bytes());
            bytes.extend_from_slice(ending.as_bytes());
        }
        bytes
    }
}

/// Which line endings a sample has.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Endings {
    Lf,
    CrLf,
    Mixed,
}

fn sample(rng: &mut SeededRng, endings: Endings) -> Sample {
    let count = rng.below(8) as usize + 2;
    let final_break = rng.below(4) != 0;
    let lines = (0..count)
        .map(|number| {
            let indent = " ".repeat(rng.below(3) as usize * 2);
            let tail = ["", " = 1;", " // note", " é"][rng.below(4) as usize];
            let ending = match endings {
                Endings::Lf => "\n",
                Endings::CrLf => "\r\n",
                Endings::Mixed if rng.below(2) == 0 => "\n",
                Endings::Mixed => "\r\n",
            };
            let last = number + 1 == count;
            (
                format!("{indent}item_{number}{tail}"),
                if last && !final_break { "" } else { ending },
            )
        })
        .collect();
    Sample {
        lines,
        mark: rng.below(4) == 0,
    }
}

/// A name for the file of one case of a property, in a workspace the cases
/// share.
fn case_file(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{prefix}-{}.txt", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[test]
fn property_applying_an_edit_and_then_reversing_it_restores_the_file() {
    let ws = Workspace::new("property_edit_reverse");
    let (edit, ws) = (edit(&ws), &ws);
    cases::check(
        "edit_reverse_on_disk",
        60,
        |rng| {
            let endings = if rng.below(2) == 0 {
                Endings::Lf
            } else {
                Endings::CrLf
            };
            let sample = sample(rng, endings);
            // One, two or three lines in a row, and what replaces them.
            let first = rng.below(sample.lines.len() as u64) as usize;
            let last = (first + rng.below(3) as usize).min(sample.lines.len() - 1);
            let lines = rng.below(3) + 1;
            (sample, first, last, lines)
        },
        |(sample, first, last, lines)| {
            let path = case_file("reverse");
            let original = sample.bytes();
            seen_file(ws, &path, &original);
            let old: Vec<&str> = sample.lines[*first..=*last]
                .iter()
                .map(|line| line.0.as_str())
                .collect();
            let old = old.join("\n");
            let new: Vec<String> = (0..*lines).map(|n| format!("replaced_{n}")).collect();
            let new = new.join("\n");

            let forward = ws.call(&edit, one_edit(&path, &old, &new));
            ensure!(!forward.is_error(), "forward: {}", forward.text());
            let between = ws.read_bytes(&path);
            ensure!(between != original, "the edit changed nothing");
            let has_crlf = |bytes: &[u8]| bytes.windows(2).any(|pair| pair == b"\r\n");
            if has_crlf(&original) && !has_crlf(&between) {
                // Every line break of the file was replaced away. What is
                // left has no line ending to go by, so the text written
                // back into it gets the default, not the original's kind.
                ws.checkpoints.clear();
                return Ok(());
            }
            // What the round kept is the file as it was, to the byte.
            let changes = ws.checkpoints.changes();
            let kept = changes
                .iter()
                .find(|change| change.file == abs(ws, &path))
                .and_then(|change| change.before.clone());
            ensure!(kept.as_ref() == Some(&original), "kept {kept:?}");

            let backward = ws.call(&edit, one_edit(&path, &new, &old));
            ensure!(!backward.is_error(), "backward: {}", backward.text());
            let restored = ws.read_bytes(&path);
            ensure!(
                restored == original,
                "restored {:?}\n  original {:?}",
                String::from_utf8_lossy(&restored),
                String::from_utf8_lossy(&original)
            );
            ws.checkpoints.clear();
            Ok(())
        },
    );
}

#[test]
fn property_bytes_outside_an_edit_survive_whatever_the_line_endings() {
    let ws = Workspace::new("property_edit_endings");
    let (edit, ws) = (edit(&ws), &ws);
    cases::check(
        "edit_endings_on_disk",
        60,
        |rng| {
            let endings = match rng.below(3) {
                0 => Endings::Lf,
                1 => Endings::CrLf,
                _ => Endings::Mixed,
            };
            let sample = sample(rng, endings);
            let line = rng.below(sample.lines.len() as u64) as usize;
            let breaks = rng.below(3);
            (sample, line, breaks)
        },
        |(sample, line, breaks)| {
            let path = case_file("endings");
            seen_file(ws, &path, &sample.bytes());
            // The word that marks the line is replaced, by text with line
            // breaks in it.
            let old = format!("item_{line}");
            let new = format!("new{}", "\nmore".repeat(*breaks as usize));
            let result = ws.call(&edit, one_edit(&path, &old, &new));
            ensure!(!result.is_error(), "{}", result.text());

            // The same, worked out from the lines: only the word changes,
            // and each new line break is written as the line's own ending,
            // or where the line has none, as most of the file's are.
            let crlf = sample.lines.iter().filter(|line| line.1 == "\r\n").count();
            let lf = sample.lines.iter().filter(|line| line.1 == "\n").count();
            let dominant = if crlf > lf || (crlf == lf && sample.lines[0].1 == "\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let mut wanted = sample.clone();
            let ending = match wanted.lines[*line].1 {
                "" => dominant,
                ending => ending,
            };
            wanted.lines[*line].0 = wanted.lines[*line]
                .0
                .replace(&old, &new.replace('\n', ending));
            let written = ws.read_bytes(&path);
            ensure!(
                written == wanted.bytes(),
                "written {:?}\n  wanted  {:?}",
                String::from_utf8_lossy(&written),
                String::from_utf8_lossy(&wanted.bytes())
            );
            ws.checkpoints.clear();
            Ok(())
        },
    );
}

/// The ways a call is made to fail.
#[derive(Debug, Clone, Copy)]
enum Mistake {
    NotFound,
    Ambiguous,
    WrongCount,
    Overlapping,
    OneBadAmongGood,
    Unseen,
    Stale,
    NoCheckpoint,
}

const MISTAKES: [Mistake; 8] = [
    Mistake::NotFound,
    Mistake::Ambiguous,
    Mistake::WrongCount,
    Mistake::Overlapping,
    Mistake::OneBadAmongGood,
    Mistake::Unseen,
    Mistake::Stale,
    Mistake::NoCheckpoint,
];

#[test]
fn property_a_failed_call_leaves_the_file_byte_identical() {
    let ws = Workspace::new("property_edit_failed");
    let ws = &ws;
    let broken = Editing::new(Arc::new(BrokenCheckpoints), Arc::new(ws.clock.clone()));
    cases::check(
        "edit_failed_leaves_the_file",
        80,
        |rng| {
            let endings = match rng.below(3) {
                0 => Endings::Lf,
                1 => Endings::CrLf,
                _ => Endings::Mixed,
            };
            let sample = sample(rng, endings);
            let mistake = MISTAKES[rng.below(MISTAKES.len() as u64) as usize];
            (sample, mistake, rng.below(2) == 0)
        },
        |(sample, mistake, through_write)| {
            let path = case_file("failed");
            let original = sample.bytes();
            ws.bytes(&path, &original);
            if !matches!(mistake, Mistake::Unseen) {
                read(ws, &path);
            }
            let on_disk = match mistake {
                Mistake::Stale => {
                    // The user changes the file after the model read it.
                    let mut changed = original.clone();
                    changed.extend_from_slice(b"the user's new line\n");
                    ws.bytes(&path, &changed);
                    changed
                }
                _ => original.clone(),
            };
            let table_before = ws.files.snapshot(&abs(ws, &path));
            let first = &sample.lines[0].0;
            let edits = match mistake {
                Mistake::NotFound => json!([{"old": "no such text anywhere", "new": "x"}]),
                // `item_` is on every line, and there are at least two.
                Mistake::Ambiguous => json!([{"old": "item_", "new": "x"}]),
                Mistake::WrongCount => json!([{"old": first, "new": "x", "count": 2}]),
                Mistake::Overlapping => json!([
                    {"old": "item_0", "new": "x"},
                    {"old": "tem_0", "new": "y"}
                ]),
                Mistake::OneBadAmongGood => json!([
                    {"old": "item_0", "new": "x"},
                    {"old": "item_1", "new": "y"},
                    {"old": "item_999", "new": "z"}
                ]),
                Mistake::Unseen | Mistake::Stale | Mistake::NoCheckpoint => {
                    json!([{"old": "item_0", "new": "x"}])
                }
            };
            let editing = if matches!(mistake, Mistake::NoCheckpoint) {
                broken.clone()
            } else {
                ws.editing()
            };
            // The guards are `write`'s too.
            let by_write = *through_write
                && matches!(
                    mistake,
                    Mistake::Unseen | Mistake::Stale | Mistake::NoCheckpoint
                );
            let result = if by_write {
                ws.call(
                    &WriteTool::new(editing),
                    json!({"path": path, "content": "all new\n"}),
                )
            } else {
                ws.call(
                    &EditTool::new(editing),
                    json!({"path": path, "edits": edits}),
                )
            };
            ensure!(
                result.outcome() == &ToolOutcome::Error,
                "the call did not fail: {}",
                result.text()
            );
            let now = ws.read_bytes(&path);
            ensure!(
                now == on_disk,
                "the file changed: {:?}",
                String::from_utf8_lossy(&now)
            );
            ensure!(
                ws.files.snapshot(&abs(ws, &path)) == table_before,
                "the table of seen files changed"
            );
            ensure!(
                ws.checkpoints.changes().is_empty(),
                "a change was recorded: {:?}",
                ws.checkpoints.changes()
            );
            let stray: Vec<String> = ws
                .listing()
                .into_iter()
                .filter(|name| name.contains(".tmp") || name.starts_with(".cortex"))
                .collect();
            ensure!(stray.is_empty(), "left behind: {stray:?}");
            Ok(())
        },
    );
}

#[test]
fn property_what_a_round_kept_puts_every_changed_file_back() {
    // The checkpoint seam is given the bytes a tool replaced. Whatever a
    // run of writes and edits does to a set of files, writing those bytes
    // back (and removing what did not exist) restores the workspace.
    let ws = Workspace::new("property_edit_undo");
    let ws = &ws;
    cases::check(
        "edit_round_undo",
        40,
        |rng| {
            let files: Vec<Sample> = (0..3)
                .map(|_| {
                    let endings = match rng.below(3) {
                        0 => Endings::Lf,
                        1 => Endings::CrLf,
                        _ => Endings::Mixed,
                    };
                    sample(rng, endings)
                })
                .collect();
            let steps: Vec<(u64, u64)> = (0..rng.below(6) + 1)
                .map(|_| (rng.below(4), rng.below(3)))
                .collect();
            (files, steps)
        },
        |(files, steps)| {
            let names: Vec<String> = (0..4).map(|_| case_file("undo")).collect();
            for (name, sample) in names.iter().zip(files) {
                seen_file(ws, name, &sample.bytes());
            }
            // The fourth name is a file that does not exist yet.
            let before: Vec<Option<Vec<u8>>> = names
                .iter()
                .map(|name| fs::read(ws.root().join(name)).ok())
                .collect();
            ws.checkpoints.clear();

            for (file, kind) in steps {
                let name = &names[*file as usize];
                let result = match (*file, *kind) {
                    (3, _) | (_, 0) => ws.call(
                        &write(ws),
                        json!({"path": name, "content": format!("rewritten {kind}\nby a tool\n")}),
                    ),
                    (_, 1) => ws.call(&edit(ws), one_edit(name, "item_0", "changed_0")),
                    _ => ws.call(&edit(ws), one_edit(name, "item_1", "changed_1\nand more")),
                };
                // A step may fail: an edit of text an earlier step removed.
                let _ = result;
            }

            // Undo, from what the round kept.
            for change in ws.checkpoints.changes() {
                let path = change.file.to_fs_path();
                match &change.before {
                    Some(content) => fs::write(&path, content).unwrap(),
                    None => {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
            for (name, before) in names.iter().zip(&before) {
                let now = fs::read(ws.root().join(name)).ok();
                ensure!(
                    now == *before,
                    "`{name}` was not put back: {:?}, was {:?}",
                    now.as_deref().map(String::from_utf8_lossy),
                    before.as_deref().map(String::from_utf8_lossy)
                );
            }
            ws.checkpoints.clear();
            // The table of seen files must not say these are current.
            for name in &names {
                let _ = fs::remove_file(ws.root().join(name));
                ws.files.forget(&abs(ws, name));
            }
            Ok(())
        },
    );
}
