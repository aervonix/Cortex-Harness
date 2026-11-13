//! Contract tests for `read`: every row of its section in
//! `docs/architecture/05-tools.md`, and the re-read table of
//! `docs/architecture/15-efficiency.md`.

use std::fs;
use std::sync::Arc;

use cortex_base::{AbsPath, ContentHash, PathOptions};
use cortex_protocol::message::Part;
use cortex_tools::{
    Concurrency, ConcurrencyClass, Effect, ImageSupport, Outliner, ReadTool, Staleness, Tool,
};
use serde_json::json;

use crate::{Workspace, failed, ok};

fn numbered(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

// --- The arguments, and what comes back -----------------------------------

#[test]
fn a_file_comes_back_as_a_header_and_its_lines_with_no_prefixes() {
    let ws = Workspace::new("read_whole");
    ws.file("src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n");
    let result = ws.call(&ReadTool::new(), json!({"path": "src/main.rs"}));
    assert_eq!(
        ok(&result),
        "src/main.rs (3 lines)\nfn main() {\n    println!(\"hi\");\n}"
    );
    assert_eq!(
        result.payload(),
        Some(&json!({
            "path": "src/main.rs",
            "kind": "content",
            "first_line": 1,
            "last_line": 3,
            "total_lines": 3,
        }))
    );
    assert!(result.artifacts().is_empty());
}

#[test]
fn offset_and_limit_return_a_range_and_the_header_says_which() {
    let ws = Workspace::new("read_range");
    ws.file("notes.txt", &numbered(50));
    let tool = ReadTool::new();
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"path": "notes.txt", "offset": 10, "limit": 3})
        )),
        "notes.txt (lines 10-12 of 50)\nline 10\nline 11\nline 12"
    );
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"path": "notes.txt", "offset": 49, "fresh": true})
        )),
        "notes.txt (lines 49-50 of 50)\nline 49\nline 50"
    );
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"path": "notes.txt", "limit": 1, "fresh": true})
        )),
        "notes.txt (line 1 of 50)\nline 1"
    );
    // A range that runs past the end stops at the end.
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"path": "notes.txt", "offset": 50, "limit": 100, "fresh": true})
        )),
        "notes.txt (line 50 of 50)\nline 50"
    );
}

#[test]
fn an_offset_past_the_end_says_how_long_the_file_is() {
    let ws = Workspace::new("read_past_end");
    ws.file("notes.txt", &numbered(5));
    ws.file("empty.txt", "");
    let tool = ReadTool::new();
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "notes.txt", "offset": 6}))),
        "`notes.txt` has 5 lines, so there is no line 6. Read it without `offset`, or with a \
         smaller one."
    );
    assert_eq!(
        ok(&ws.call(&tool, json!({"path": "empty.txt"}))),
        "empty.txt (empty: 0 lines)"
    );
    assert!(failed(&ws.call(&tool, json!({"path": "empty.txt", "offset": 2}))).contains("0 lines"));
}

#[test]
fn arguments_that_do_not_fit_are_refused_before_anything_is_read() {
    let ws = Workspace::new("read_arguments");
    ws.file("a.txt", "a\n");
    let tool = ReadTool::new();
    for (arguments, expected) in [
        (
            json!({"path": "a.txt", "offset": 0}),
            "`offset` must be 1 or more, and is 0",
        ),
        (
            json!({"path": "a.txt", "limit": 0}),
            "`limit` must be 1 or more, and is 0",
        ),
        (
            json!({"path": 7}),
            "`path` must be a string, and is a number (7)",
        ),
        (
            json!({"path": "a.txt", "lines": 3}),
            "`lines` is not an argument this takes",
        ),
        (
            json!({"path": "a.txt", "fresh": "yes"}),
            "`fresh` must be a boolean",
        ),
        (json!({}), "`read` needs `path` (a file) or `artifact`"),
        (
            json!({"path": "a.txt", "artifact": "a_0001"}),
            "`read` takes `path` or `artifact`, not both",
        ),
    ] {
        let text = failed(&ws.call(&tool, arguments.clone()));
        assert!(text.contains(expected), "{arguments}: {text}");
    }
    assert_eq!(ws.files.seen().len(), 0, "nothing was read");
}

#[test]
fn line_endings_and_a_byte_order_mark_are_not_shown() {
    let ws = Workspace::new("read_crlf");
    ws.bytes("win.txt", b"\xef\xbb\xbffirst\r\nsecond\r\n");
    assert_eq!(
        ok(&ws.call(&ReadTool::new(), json!({"path": "win.txt"}))),
        "win.txt (2 lines)\nfirst\nsecond"
    );
}

#[test]
fn a_file_that_is_not_utf8_is_shown_with_a_warning() {
    let ws = Workspace::new("read_latin1");
    ws.bytes("old.txt", b"caf\xe9\nplain\n");
    assert_eq!(
        ok(&ws.call(&ReadTool::new(), json!({"path": "old.txt"}))),
        "old.txt (2 lines)\ncaf\u{fffd}\nplain\n[This file is not valid UTF-8. The bytes that \
         are not are shown as \u{fffd}, and the editing tools will refuse the file.]"
    );
}

// --- Large files -----------------------------------------------------------

#[test]
fn a_read_cut_by_the_output_policy_says_where_to_continue() {
    let ws = Workspace::new("read_cut");
    ws.file("big.txt", &numbered(5000));
    let tool = ReadTool::new();
    let text = ok(&ws.call(&tool, json!({"path": "big.txt"})));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "big.txt (lines 1-796 of 5,000)");
    assert_eq!(lines[796], "line 796");
    assert_eq!(
        lines[797],
        "[Stopped before line 797: the output limit was reached. 4,204 lines not shown: \
         continue with `offset: 797`.]"
    );
    assert_eq!(lines.len(), 798, "within the 800 lines of the policy");

    // Continuing there gives the lines that follow.
    let next = ok(&ws.call(&tool, json!({"path": "big.txt", "offset": 797})));
    assert!(
        next.starts_with("big.txt (lines 797-1592 of 5,000)\nline 797\n"),
        "{next}"
    );
    assert!(next.contains("continue with `offset: 1593`"), "{next}");
}

#[test]
fn the_byte_limit_stops_a_read_as_the_line_limit_does() {
    let ws = Workspace::new("read_cut_bytes");
    // 300 lines of 200 characters: under 800 lines, over 32 KiB.
    let line = "x".repeat(199);
    ws.file("wide.txt", &format!("{line}\n").repeat(300));
    let text = ok(&ws.call(&ReadTool::new(), json!({"path": "wide.txt"})));
    assert!(text.len() <= 32 * 1024);
    let shown = text.lines().filter(|shown| *shown == line).count();
    assert!((150..300).contains(&shown), "{shown} lines shown");
    assert!(
        text.ends_with(&format!(
            "[Stopped before line {}: the output limit was reached. {} lines not shown: \
             continue with `offset: {}`.]",
            shown + 1,
            300 - shown,
            shown + 1
        )),
        "{}",
        text.lines().last().unwrap()
    );
}

#[test]
fn a_range_of_a_large_file_is_read_without_holding_the_file() {
    let ws = Workspace::new("read_stream");
    // Twelve megabytes, with a line of two megabytes in the middle.
    let mut content = String::new();
    for n in 1..=100_000 {
        content.push_str(&format!("{n:>8} {}\n", "padding ".repeat(12)));
    }
    content.push_str(&"long ".repeat(400_000));
    content.push('\n');
    content.push_str("the end\n");
    ws.file("huge.log", &content);

    let tool = ReadTool::new();
    let text = ok(&ws.call(
        &tool,
        json!({"path": "huge.log", "offset": 100_000, "limit": 3}),
    ));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "huge.log (lines 100000-100002 of 100,002)");
    assert!(lines[1].starts_with("  100000 padding"));
    assert!(lines[2].starts_with("long long long"));
    assert!(
        lines[2].ends_with("characters]"),
        "the long line is shortened: {}",
        &lines[2][lines[2].len() - 40..]
    );
    assert!(lines[2].chars().count() <= 2000);
    assert_eq!(lines[3], "the end");

    // The snapshot is of the whole file, though only three lines were kept.
    let snapshot = ws
        .files
        .snapshot(ws.paths.resolve("huge.log").unwrap().abs())
        .unwrap();
    assert_eq!(snapshot.size, content.len() as u64);
    assert_eq!(snapshot.hash, ContentHash::of(content.as_bytes()));
    // A file this large is not kept as a version to diff against.
    assert_eq!(ws.store.version_count(), 0);
}

/// An outline source that knows one file.
struct OneOutline(&'static str);

impl Outliner for OneOutline {
    fn outline(&self, path: &AbsPath) -> Option<String> {
        (path.file_name() == Some("big.rs")).then(|| self.0.to_owned())
    }
}

#[test]
fn a_large_file_is_introduced_by_its_outline_then_its_first_lines() {
    let ws = Workspace::new("read_outline");
    ws.file("big.rs", &numbered(5000));
    ws.file("other.rs", &numbered(5000));
    let cx = ws.cx().with_outliner(Arc::new(OneOutline(
        "fn alpha()  [line 10]\nfn beta()  [line 2400]",
    )));
    let tool = ReadTool::new();

    let result = ws.call_with(&cx, &tool, json!({"path": "big.rs"}));
    let text = ok(&result);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        &lines[..4],
        [
            "big.rs (5,000 lines, 47.7 KiB) is too long to return whole. Its outline, with line numbers:",
            "fn alpha()  [line 10]",
            "fn beta()  [line 2400]",
            "big.rs (lines 1-792 of 5,000)",
        ]
    );
    assert_eq!(lines[4], "line 1");
    assert_eq!(
        *lines.last().unwrap(),
        "[Stopped before line 793: the output limit was reached. 4,208 lines not shown: \
         continue with `offset: 793`.]"
    );
    assert_eq!(result.payload().unwrap()["kind"], "outline");

    // A range was asked for: no outline. A file with no outline: none.
    let ranged = ok(&ws.call_with(
        &cx,
        &tool,
        json!({"path": "big.rs", "offset": 2400, "limit": 5}),
    ));
    assert!(
        ranged.starts_with("big.rs (lines 2400-2404 of 5,000)\n"),
        "{ranged}"
    );
    let plain = ok(&ws.call_with(&cx, &tool, json!({"path": "other.rs"})));
    assert!(
        plain.starts_with("other.rs (lines 1-796 of 5,000)\n"),
        "{plain}"
    );
    // A file that fits is returned whole, outline or not.
    ws.file("small/big.rs", &numbered(3));
    let small = ok(&ws.call_with(&cx, &tool, json!({"path": "small/big.rs"})));
    assert_eq!(small, "small/big.rs (3 lines)\nline 1\nline 2\nline 3");
}

// --- Re-reads --------------------------------------------------------------

#[test]
fn rereading_an_unchanged_file_returns_one_line() {
    let ws = Workspace::new("reread_unchanged");
    ws.file("a.txt", &numbered(120));
    let tool = ReadTool::new();
    let cx = ws.cx().with_turn(3);
    let first = ok(&ws.call_with(&cx, &tool, json!({"path": "a.txt"})));
    assert!(first.starts_with("a.txt (120 lines)\nline 1\n"));

    let later = ws.cx().with_turn(5);
    let again = ws.call_with(&later, &tool, json!({"path": "a.txt"}));
    assert_eq!(
        ok(&again),
        "a.txt: unchanged since you read lines 1-120 in turn 3 (120 lines in all). The content \
         is not sent again: pass `fresh: true` if you need it."
    );
    assert_eq!(again.payload().unwrap()["kind"], "unchanged");

    // A part of what was read is unchanged too.
    let part = ok(&ws.call_with(
        &later,
        &tool,
        json!({"path": "a.txt", "offset": 40, "limit": 10}),
    ));
    assert!(
        part.starts_with("a.txt: unchanged since you read lines 1-120 in turn 3"),
        "{part}"
    );
}

#[test]
fn fresh_returns_the_content_of_an_unchanged_file() {
    let ws = Workspace::new("reread_fresh");
    ws.file("a.txt", "one\ntwo\n");
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt"})));
    assert_eq!(
        ok(&ws.call(&tool, json!({"path": "a.txt", "fresh": true}))),
        "a.txt (2 lines)\none\ntwo"
    );
    // And it is still known as read afterwards.
    assert!(
        ok(&ws.call(&tool, json!({"path": "a.txt"})))
            .starts_with("a.txt: unchanged since you read lines 1-2 (")
    );
}

#[test]
fn a_range_not_seen_before_is_sent_and_then_remembered() {
    let ws = Workspace::new("reread_ranges");
    ws.file("a.txt", &numbered(100));
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 1, "limit": 10})));
    // Lines 5 to 14 are only half seen: the content is sent.
    let overlap = ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 5, "limit": 10})));
    assert!(
        overlap.starts_with("a.txt (lines 5-14 of 100)\nline 5\n"),
        "{overlap}"
    );
    // Now 1 to 14 are seen, and 50 to 59 are not.
    let seen = ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 2, "limit": 12})));
    assert!(
        seen.starts_with("a.txt: unchanged since you read lines 1-14 ("),
        "{seen}"
    );
    let far = ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 50, "limit": 10})));
    assert!(far.starts_with("a.txt (lines 50-59 of 100)\n"), "{far}");
    let both = ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 55, "limit": 2})));
    assert!(
        both.starts_with("a.txt: unchanged since you read lines 1-14, 50-59 ("),
        "{both}"
    );
    // The whole file was never seen.
    let whole = ok(&ws.call(&tool, json!({"path": "a.txt"})));
    assert!(whole.starts_with("a.txt (100 lines)\n"), "{whole}");
}

#[test]
fn a_small_change_comes_back_as_a_diff_against_what_was_seen() {
    let ws = Workspace::new("reread_diff");
    let original = numbered(200);
    ws.file("a.txt", &original);
    let tool = ReadTool::new();
    ok(&ws.call_with(&ws.cx().with_turn(2), &tool, json!({"path": "a.txt"})));

    ws.file(
        "a.txt",
        &original.replace("line 100\n", "line one hundred\nand one more\n"),
    );
    let result = ws.call_with(&ws.cx().with_turn(4), &tool, json!({"path": "a.txt"}));
    assert_eq!(
        ok(&result),
        "a.txt: changed since you read it in turn 2, and now has 201 lines. This is the \
         difference from the version you saw (+2 -1); pass `fresh: true` for the content.\n\
         @@ -97,7 +97,8 @@\n line 97\n line 98\n line 99\n-line 100\n+line one hundred\n\
         +and one more\n line 101\n line 102\n line 103"
    );
    assert_eq!(
        result.payload(),
        Some(
            &json!({"path": "a.txt", "kind": "diff", "total_lines": 201, "added": 2, "removed": 1})
        )
    );

    // The model now has the new version: a third read is "unchanged", and
    // the table holds the new snapshot for the staleness guard.
    let third = ok(&ws.call_with(&ws.cx().with_turn(6), &tool, json!({"path": "a.txt"})));
    assert!(
        third.starts_with("a.txt: unchanged since you read lines 1-201 in turn 4"),
        "{third}"
    );
    let abs = ws.paths.resolve("a.txt").unwrap();
    let on_disk = ContentHash::of(&fs::read(ws.root().join("a.txt")).unwrap());
    assert_eq!(ws.files.staleness(abs.abs(), &on_disk), Staleness::Current);
    assert_eq!(
        ws.files
            .staleness(abs.abs(), &ContentHash::of(original.as_bytes())),
        Staleness::Stale
    );
}

#[test]
fn a_large_change_comes_back_as_content() {
    let ws = Workspace::new("reread_rewritten");
    ws.file("a.txt", &numbered(40));
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt"})));
    // Every line is different: the diff would be longer than the file.
    let rewritten: String = (1..=40).map(|n| format!("row {n}\n")).collect();
    ws.file("a.txt", &rewritten);
    let result = ws.call(&tool, json!({"path": "a.txt"}));
    assert!(ok(&result).starts_with("a.txt (40 lines)\nrow 1\nrow 2\n"));
    assert_eq!(result.payload().unwrap()["kind"], "content");
}

#[test]
fn fresh_returns_the_content_of_a_changed_file_too() {
    let ws = Workspace::new("reread_fresh_changed");
    let original = numbered(200);
    ws.file("a.txt", &original);
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt"})));
    ws.file(
        "a.txt",
        &original.replace("line 100\n", "line one hundred\n"),
    );
    let text = ok(&ws.call(&tool, json!({"path": "a.txt", "fresh": true})));
    assert!(text.starts_with("a.txt (200 lines)\nline 1\n"), "{text}");
    assert!(text.contains("\nline one hundred\n"));
}

#[test]
fn a_change_of_line_endings_alone_is_said_in_one_line() {
    let ws = Workspace::new("reread_endings");
    ws.file("a.txt", &numbered(30));
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt"})));
    ws.file("a.txt", &numbered(30).replace('\n', "\r\n"));
    assert_eq!(
        ok(&ws.call(&tool, json!({"path": "a.txt"}))),
        "a.txt: only its line endings or its final line break changed since you read it. Its 30 \
         lines are the same."
    );
    // For a file shorter than that sentence, the content is the shorter
    // answer, and is what is sent.
    ws.file("tiny.txt", "one\n");
    ok(&ws.call(&tool, json!({"path": "tiny.txt"})));
    ws.file("tiny.txt", "one");
    assert_eq!(
        ok(&ws.call(&tool, json!({"path": "tiny.txt"}))),
        "tiny.txt (1 line)\none"
    );
}

#[test]
fn a_changed_file_of_which_only_part_was_seen_is_diffed_only_for_that_part() {
    let ws = Workspace::new("reread_partial");
    let original = numbered(300);
    ws.file("a.txt", &original);
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 100, "limit": 50})));
    ws.file(
        "a.txt",
        &original.replace("line 120\n", "line 120, changed\n"),
    );

    // The lines seen before: a diff.
    let seen = ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 100, "limit": 50})));
    assert!(
        seen.starts_with("a.txt: changed since you read it, and now has 300 lines."),
        "{seen}"
    );
    assert!(seen.contains("-line 120\n+line 120, changed"), "{seen}");
    // Lines never seen: the content, whatever changed elsewhere.
    let unseen = ok(&ws.call(&tool, json!({"path": "a.txt", "offset": 200, "limit": 5})));
    assert!(
        unseen.starts_with("a.txt (lines 200-204 of 300)\nline 200\n"),
        "{unseen}"
    );
}

#[test]
fn the_first_read_in_a_new_epoch_is_always_in_full() {
    let ws = Workspace::new("reread_epoch");
    ws.file("a.txt", "one\ntwo\n");
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt"})));
    // Compaction: the model no longer has the content.
    ws.files.begin_epoch();
    assert_eq!(
        ok(&ws.call(&tool, json!({"path": "a.txt"}))),
        "a.txt (2 lines)\none\ntwo"
    );
    assert!(ok(&ws.call(&tool, json!({"path": "a.txt"}))).starts_with("a.txt: unchanged"));
}

#[test]
fn the_version_the_model_saw_is_kept_in_the_content_store() {
    let ws = Workspace::new("reread_store");
    ws.file("a.txt", "one\ntwo\n");
    let tool = ReadTool::new();
    ok(&ws.call(&tool, json!({"path": "a.txt"})));
    let hash = ContentHash::of(b"one\ntwo\n");
    use cortex_tools::ContentStore;
    assert_eq!(
        ws.store.get_version(&hash).unwrap(),
        Some(b"one\ntwo\n".to_vec())
    );
    let snapshot = ws
        .files
        .snapshot(ws.paths.resolve("a.txt").unwrap().abs())
        .unwrap();
    assert_eq!((snapshot.size, snapshot.hash), (8, hash));
    assert!(snapshot.modified.is_some());
}

// --- What is not text ------------------------------------------------------

#[test]
fn a_binary_file_is_refused_with_its_size_and_type() {
    let ws = Workspace::new("read_binary");
    ws.bytes("tool.bin", b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0");
    ws.bytes("data.db", &[1, 2, 3, 0, 4, 5]);
    let tool = ReadTool::new();
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "tool.bin"}))),
        "`tool.bin` is not a text file: it is an ELF executable, 16 bytes. It is not read."
    );
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "data.db"}))),
        "`data.db` is not a text file: it is binary data, 6 bytes. It is not read."
    );
    assert!(
        ws.files.seen().is_empty(),
        "a refused file is not a seen file"
    );
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01";

#[test]
fn an_image_is_returned_as_an_image_when_the_model_takes_it() {
    let ws = Workspace::new("read_image");
    ws.bytes("logo.png", PNG);
    let tool = ReadTool::new();
    let cx = ws.cx().with_images(ImageSupport {
        media_types: vec!["image/png".to_owned(), "image/jpeg".to_owned()],
        max_bytes: 1024,
    });
    let result = ws.call_with(&cx, &tool, json!({"path": "logo.png"}));
    assert_eq!(ok(&result), "logo.png (image/png, 24 bytes)");
    let [Part::Text(_), Part::Image(image)] = result.content() else {
        panic!("text and an image, not {:?}", result.content());
    };
    assert_eq!(image.media_type, "image/png");
    assert_eq!(image.data, None, "the image is stored, not carried inline");
    let id = image.artifact.clone().expect("an artifact");
    assert_eq!(ws.store.artifact(&id).as_deref(), Some(PNG));
    assert_eq!(
        ws.store.artifact_info(&id),
        Some(("logo.png".to_owned(), "image/png".to_owned()))
    );
    assert_eq!(result.artifacts().len(), 1);
}

#[test]
fn an_image_the_model_cannot_take_is_refused_with_the_reason() {
    let ws = Workspace::new("read_image_refused");
    ws.bytes("logo.png", PNG);
    let tool = ReadTool::new();
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "logo.png"}))),
        "`logo.png` is an image (image/png, 24 bytes), and the model in use does not take \
         images. It cannot be read as text."
    );
    let small = ws.cx().with_images(ImageSupport {
        media_types: vec!["image/png".to_owned()],
        max_bytes: 10,
    });
    assert_eq!(
        failed(&ws.call_with(&small, &tool, json!({"path": "logo.png"}))),
        "`logo.png` is an image (image/png, 24 bytes) that the model in use does not take: it \
         takes image/png of up to 10 bytes."
    );
    assert_eq!(ws.store.artifact_count(), 0);
}

// --- What is not there -----------------------------------------------------

#[test]
fn a_missing_file_lists_close_matches_in_the_same_directory() {
    let ws = Workspace::new("read_missing");
    ws.file("src/main.rs", "");
    ws.file("src/main_test.rs", "");
    ws.file("src/lib.rs", "");
    ws.file("src/Config.toml", "");
    let tool = ReadTool::new();
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "src/mian.rs"}))),
        "There is no file at `src/mian.rs`. Close matches in `src`: main.rs."
    );
    // Where names are compared exactly, another case is a close match.
    // (Windows finds the file whatever the case it is asked in.)
    if !cfg!(windows) {
        assert_eq!(
            failed(&ws.call(&tool, json!({"path": "src/config.toml"}))),
            "There is no file at `src/config.toml`. Close matches in `src`: Config.toml."
        );
    }
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "src/main.py"}))),
        "There is no file at `src/main.py`. Close matches in `src`: main.rs, main_test.rs."
    );
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "src/zzz.json"}))),
        "There is no file at `src/zzz.json`. Nothing in `src` has a similar name: `list` it to \
         see what is there."
    );
}

#[test]
fn a_missing_directory_on_the_way_is_named() {
    let ws = Workspace::new("read_missing_dir");
    ws.file("source/deep/main.rs", "");
    assert_eq!(
        failed(&ws.call(&ReadTool::new(), json!({"path": "sorce/deep/main.rs"}))),
        "There is no file at `sorce/deep/main.rs`: `sorce` does not exist. Close matches in \
         `.`: source."
    );
}

#[test]
fn a_directory_is_not_read_and_says_what_to_use() {
    let ws = Workspace::new("read_directory");
    ws.dir("src");
    assert_eq!(
        failed(&ws.call(&ReadTool::new(), json!({"path": "src"}))),
        "`src` is a directory. Use `list` to see what is in it."
    );
}

// --- Paths, effects and scheduling ----------------------------------------

#[test]
fn a_read_declares_one_read_effect_on_the_real_path_and_runs_beside_others() {
    let ws = Workspace::new("read_plan");
    ws.file("src/main.rs", "");
    let tool = ReadTool::new();
    assert_eq!(tool.spec().name, "read");
    assert_eq!(tool.spec().concurrency, ConcurrencyClass::ReadOnly);

    let plan = ws
        .plan(
            &tool,
            json!({"path": "./src/../src/main.rs", "offset": 5, "limit": 10}),
        )
        .unwrap();
    assert_eq!(plan.summary, "Read src/main.rs, lines 5-14");
    assert_eq!(plan.concurrency, Concurrency::ReadOnly);
    assert!(!plan.has_side_effects());
    let [Effect::Read(path)] = plan.effects.as_slice() else {
        panic!("one read, not {:?}", plan.effects);
    };
    assert_eq!(path.display(), "src/main.rs");
    assert!(path.is_inside_workspace());
    assert_eq!(path.abs(), ws.paths.resolve("src/main.rs").unwrap().abs());

    let whole = ws.plan(&tool, json!({"path": "src/main.rs"})).unwrap();
    assert_eq!(whole.summary, "Read src/main.rs");
    let from = ws
        .plan(&tool, json!({"path": "src/main.rs", "offset": 7}))
        .unwrap();
    assert_eq!(from.summary, "Read src/main.rs, from line 7");
}

#[test]
fn a_path_outside_the_workspace_resolves_and_the_effect_says_so() {
    let ws = Workspace::new("read_outside");
    fs::write(ws.dir.join("beside.txt"), "outside\n").unwrap();
    let tool = ReadTool::new();
    let plan = ws.plan(&tool, json!({"path": "../beside.txt"})).unwrap();
    let [Effect::Read(path)] = plan.effects.as_slice() else {
        panic!("one read, not {:?}", plan.effects);
    };
    assert!(!path.is_inside_workspace(), "the permission engine is told");
    assert!(
        path.display().ends_with("/read_outside/beside.txt"),
        "{}",
        path.display()
    );
    // The tool itself reads what the plan names. Whether it may is decided
    // by the permission engine, on the effect.
    let text = ok(&ws.call(&tool, json!({"path": "../beside.txt"})));
    assert!(
        text.ends_with("/read_outside/beside.txt (1 line)\noutside"),
        "{text}"
    );
}

#[test]
fn a_path_the_resolver_refuses_is_refused_with_the_resolvers_reason() {
    let ws = Workspace::new("read_refused");
    let tool = ReadTool::new();
    let nul = failed(&ws.call(&tool, json!({"path": "a\u{0}b"})));
    assert_eq!(nul, "the path `a\\0b` contains a NUL character");
    // `~` with no home directory known.
    let home = failed(&ws.call(&tool, json!({"path": "~/notes.txt"})));
    assert_eq!(
        home,
        "`~/notes.txt` begins with `~`, and the home directory is not known"
    );
    // The plan fails: there is nothing to approve and nothing to run.
    assert!(ws.plan(&tool, json!({"path": "~/notes.txt"})).is_err());
}

#[test]
fn a_path_under_the_home_directory_is_read_through_the_tilde() {
    let home = Workspace::new("read_home_dir");
    home.file("notes.txt", "from home\n");
    let home_path = AbsPath::from_os(&fs::canonicalize(home.root()).unwrap()).unwrap();
    let ws = Workspace::with_options("read_home", PathOptions::new().home(home_path));
    let text = ok(&ws.call(&ReadTool::new(), json!({"path": "~/notes.txt"})));
    assert!(
        text.ends_with("/read_home_dir/ws/notes.txt (1 line)\nfrom home"),
        "{text}"
    );
}

// --- Artifacts --------------------------------------------------------------

#[test]
fn an_artifact_is_read_by_its_id_with_the_same_ranges() {
    use cortex_tools::ContentStore;
    let ws = Workspace::new("read_artifact");
    let artifact = ws
        .store
        .put_artifact(
            "full output of shell",
            "text/plain",
            numbered(3000).as_bytes(),
        )
        .unwrap();
    let tool = ReadTool::new();
    let id = artifact.id.as_str();

    let plan = ws
        .plan(&tool, json!({"artifact": id, "offset": 2000, "limit": 2}))
        .unwrap();
    assert_eq!(plan.summary, format!("Read artifact {id}, lines 2000-2001"));
    assert!(
        plan.effects.is_empty(),
        "the session's own record, not a file"
    );

    assert_eq!(
        ok(&ws.call(&tool, json!({"artifact": id, "offset": 2000, "limit": 2}))),
        format!("artifact {id} (lines 2000-2001 of 3,000)\nline 2000\nline 2001")
    );
    let whole = ok(&ws.call(&tool, json!({"artifact": id})));
    assert!(
        whole.contains("continue with `offset: 797`"),
        "an artifact is bounded like a file"
    );
    // Reading it again sends it again: an artifact is not a seen file.
    assert!(ok(&ws.call(&tool, json!({"artifact": id, "limit": 1}))).ends_with("\nline 1"));
    assert!(ws.files.seen().is_empty());

    assert_eq!(
        failed(&ws.call(&tool, json!({"artifact": "a_9999"}))),
        "There is no artifact `a_9999` in this session. An artifact id is given in the notice \
         at the end of a result that was cut."
    );
}
