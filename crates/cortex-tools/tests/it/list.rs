//! Contract tests for `list`: every row of its section in
//! `docs/architecture/05-tools.md`.

use std::sync::Arc;

use cortex_base::AbsPath;
use cortex_tools::{Concurrency, ConcurrencyClass, Effect, ListTool, OutputLimits, ReadGate, Tool};
use serde_json::json;

use crate::{Workspace, failed, ok};

/// A small project: sources, a build directory that is ignored, a hidden
/// file.
fn project(test: &str) -> Workspace {
    let ws = Workspace::new(test);
    ws.file(".gitignore", "target/\n*.log\n");
    ws.file("Cargo.toml", "");
    ws.file("README.md", "");
    ws.file("build.log", "");
    ws.file(".env", "");
    ws.file("src/main.rs", "");
    ws.file("src/lib.rs", "");
    ws.file("src/util/text.rs", "");
    ws.file("src/util/deep/more.rs", "");
    ws.file("tests/it/main.rs", "");
    ws.file("target/debug/app", "");
    ws
}

#[test]
fn a_directory_is_listed_one_entry_to_a_line_sorted_with_directories_marked() {
    let ws = project("list_root");
    let result = ws.call(&ListTool::new(), json!({}));
    assert_eq!(
        ok(&result),
        "4 entries in `.`:\nCargo.toml\nREADME.md\nsrc/\ntests/"
    );
    assert_eq!(
        result.payload(),
        Some(&json!({"path": ".", "entries": 4, "shown": 4}))
    );
}

#[test]
fn entries_are_paths_that_other_tools_take_as_they_are() {
    let ws = project("list_sub");
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({"path": "src"}))),
        "3 entries in `src`:\nsrc/lib.rs\nsrc/main.rs\nsrc/util/"
    );
    // Any spelling of the directory gives the same paths.
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({"path": "./src/util/../"}))),
        "3 entries in `src`:\nsrc/lib.rs\nsrc/main.rs\nsrc/util/"
    );
}

#[test]
fn depth_goes_further_down() {
    let ws = project("list_depth");
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({"path": "src", "depth": 2}))),
        "5 entries in `src`:\nsrc/lib.rs\nsrc/main.rs\nsrc/util/\nsrc/util/deep/\nsrc/util/text.rs"
    );
}

#[test]
fn a_glob_finds_files_at_any_depth() {
    let ws = project("list_glob");
    let tool = ListTool::new();
    assert_eq!(
        ok(&ws.call(&tool, json!({"glob": "**/*.rs"}))),
        "5 entries matching `**/*.rs` under `.`:\nsrc/lib.rs\nsrc/main.rs\n\
         src/util/deep/more.rs\nsrc/util/text.rs\ntests/it/main.rs"
    );
    // A glob with no separator matches a name anywhere.
    assert_eq!(
        ok(&ws.call(&tool, json!({"glob": "main.rs"}))),
        "2 entries matching `main.rs` under `.`:\nsrc/main.rs\ntests/it/main.rs"
    );
    // One with a separator matches the path below the directory listed.
    assert_eq!(
        ok(&ws.call(&tool, json!({"path": "src", "glob": "util/*.rs"}))),
        "1 entry matching `util/*.rs` under `src`:\nsrc/util/text.rs"
    );
    // A glob can match directories too.
    assert_eq!(
        ok(&ws.call(&tool, json!({"glob": "**/util"}))),
        "1 entry matching `**/util` under `.`:\nsrc/util/"
    );
    // And depth limits a glob as it limits a listing.
    assert_eq!(
        ok(&ws.call(&tool, json!({"glob": "*.rs", "depth": 2}))),
        "2 entries matching `*.rs` under `.`:\nsrc/lib.rs\nsrc/main.rs"
    );
}

#[test]
fn ignored_and_hidden_files_are_left_out_unless_asked_for() {
    let ws = project("list_ignored");
    let tool = ListTool::new();
    let all = ok(&ws.call(&tool, json!({"include_ignored": true})));
    assert_eq!(
        all,
        "8 entries in `.`:\n.env\n.gitignore\nCargo.toml\nREADME.md\nbuild.log\nsrc/\ntarget/\ntests/"
    );
    assert_eq!(
        ok(&ws.call(&tool, json!({"glob": "*.log"}))),
        "Nothing under `.` matches `*.log`. Hidden and ignored files are left out: \
         `include_ignored: true` lists them too."
    );
    assert_eq!(
        ok(&ws.call(&tool, json!({"glob": "*.log", "include_ignored": true}))),
        "1 entry matching `*.log` under `.`:\nbuild.log"
    );
}

#[test]
fn an_ignore_file_of_the_harness_s_own_kind_is_honoured_too() {
    let ws = Workspace::new("list_dot_ignore");
    ws.file(".ignore", "generated/\n");
    ws.file("generated/big.json", "");
    ws.file("kept.txt", "");
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({}))),
        "1 entry in `.`:\nkept.txt"
    );
}

#[test]
fn an_empty_directory_says_so() {
    let ws = Workspace::new("list_empty");
    ws.dir("empty");
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({"path": "empty"}))),
        "`empty` is empty. Hidden and ignored files are left out: `include_ignored: true` \
         lists them too."
    );
    assert_eq!(
        ok(&ws.call(
            &ListTool::new(),
            json!({"path": "empty", "include_ignored": true})
        )),
        "`empty` is empty."
    );
}

#[test]
fn max_results_cuts_the_list_and_says_how_many_more_there_are() {
    let ws = Workspace::new("list_max");
    for n in 0..30 {
        ws.file(&format!("f{n:02}.txt"), "");
    }
    let result = ws.call(&ListTool::new(), json!({"max_results": 3}));
    assert_eq!(
        ok(&result),
        "The first 3 of 30 entries in `.`:\nf00.txt\nf01.txt\nf02.txt\n\
         [27 more not shown: `max_results` was reached. Narrow `path` or `glob` to see them.]"
    );
    assert_eq!(
        result.payload(),
        Some(&json!({"path": ".", "entries": 30, "shown": 3}))
    );
}

#[test]
fn the_default_is_two_hundred_entries() {
    let ws = Workspace::new("list_default_max");
    for n in 0..230 {
        ws.file(&format!("f{n:03}.txt"), "");
    }
    let text = ok(&ws.call(&ListTool::new(), json!({})));
    assert!(
        text.starts_with("The first 200 of 230 entries in `.`:\nf000.txt\n"),
        "{text}"
    );
    assert!(text.ends_with("f199.txt\n[30 more not shown: `max_results` was reached. Narrow `path` or `glob` to see them.]"), "{text}");
}

#[test]
fn the_output_limits_cut_a_list_that_max_results_would_let_through() {
    let ws = Workspace::new("list_limits");
    for n in 0..60 {
        ws.file(&format!("f{n:02}.txt"), "");
    }
    let cx = ws.cx().with_limits(OutputLimits::new(1 << 20, 20, 2000));
    let text = ok(&ws.call_with(&cx, &ListTool::new(), json!({})));
    assert!(
        text.starts_with("The first 12 of 60 entries in `.`:\nf00.txt\n"),
        "{text}"
    );
    assert!(
        text.ends_with("[48 more not shown: the output limit was reached. Narrow `path` or `glob` to see them.]"),
        "{text}"
    );
}

#[test]
fn what_is_not_a_directory_is_not_listed() {
    let ws = project("list_not_dir");
    let tool = ListTool::new();
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "Cargo.toml"}))),
        "`Cargo.toml` is a file, not a directory. Use `read` to see what is in it."
    );
    assert_eq!(
        failed(&ws.call(&tool, json!({"path": "nowhere"}))),
        "There is no directory at `nowhere`. `list` its parent to see what is there."
    );
}

#[test]
fn arguments_that_do_not_fit_are_refused() {
    let ws = project("list_arguments");
    let tool = ListTool::new();
    for (arguments, expected) in [
        (json!({"depth": 0}), "`depth` must be 1 or more, and is 0"),
        (
            json!({"max_results": 0}),
            "`max_results` must be 1 or more, and is 0",
        ),
        (
            json!({"max_results": 100_000}),
            "`max_results` must be 5000 or less",
        ),
        (json!({"glob": "src/["}), "`glob` is not a valid glob"),
        (json!({"path": ""}), "`path` must not be empty"),
        (
            json!({"hidden": true}),
            "`hidden` is not an argument this takes",
        ),
    ] {
        let text = failed(&ws.call(&tool, arguments.clone()));
        assert!(text.contains(expected), "{arguments}: {text}");
    }
}

#[test]
fn a_listing_is_a_read_of_one_directory_and_a_search_by_glob_a_walk_of_the_tree() {
    let ws = project("list_plan");
    let tool = ListTool::new();
    assert_eq!(tool.spec().name, "list");
    assert_eq!(tool.spec().concurrency, ConcurrencyClass::ReadOnly);

    let plan = ws.plan(&tool, json!({"path": "src"})).unwrap();
    assert_eq!(plan.summary, "List src");
    assert_eq!(plan.concurrency, Concurrency::ReadOnly);
    assert!(matches!(plan.effects.as_slice(), [Effect::Read(path)] if path.display() == "src"));

    let plan = ws
        .plan(&tool, json!({"path": "src", "glob": "**/*.rs"}))
        .unwrap();
    assert_eq!(plan.summary, "Find **/*.rs under src");
    assert!(matches!(plan.effects.as_slice(), [Effect::ReadTree(path)] if path.display() == "src"));
    assert!(!plan.has_side_effects());

    let plan = ws.plan(&tool, json!({"depth": 3})).unwrap();
    assert!(matches!(plan.effects.as_slice(), [Effect::ReadTree(path)] if path.display() == "."));
}

/// A gate that keeps anything named `secrets` out of a walk.
struct NoSecrets;

impl ReadGate for NoSecrets {
    fn may_read(&self, path: &AbsPath) -> bool {
        path.file_name() != Some("secrets")
    }
}

#[test]
fn an_entry_the_permission_rules_protect_is_stepped_past_and_counted() {
    let ws = project("list_gate");
    ws.file("secrets/key.pem", "");
    ws.file("src/secrets", "");
    let cx = ws.cx().with_gate(Arc::new(NoSecrets));
    assert_eq!(
        ok(&ws.call_with(&cx, &ListTool::new(), json!({"glob": "**/*"}))),
        "12 entries matching `**/*` under `.`:\nCargo.toml\nREADME.md\nsrc/\nsrc/lib.rs\n\
         src/main.rs\nsrc/util/\nsrc/util/deep/\nsrc/util/deep/more.rs\nsrc/util/text.rs\n\
         tests/\ntests/it/\ntests/it/main.rs\n\
         [Skipped 2 entries that the permission rules do not let this call read.]"
    );
}

#[test]
fn listing_stops_when_cancelled() {
    let ws = project("list_cancel");
    let cx = ws.cx();
    cx.cancel.cancel();
    let result = ws.call_with(&cx, &ListTool::new(), json!({"glob": "**/*"}));
    assert_eq!(
        result.outcome(),
        &cortex_protocol::item::ToolOutcome::Interrupted
    );
    assert_eq!(result.text(), "`list` was interrupted before it finished.");
}
