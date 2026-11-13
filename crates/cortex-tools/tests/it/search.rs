//! Contract tests for `search`: every row of its section in
//! `docs/architecture/05-tools.md`.

use cortex_protocol::item::ToolOutcome;
use cortex_tools::{
    Concurrency, ConcurrencyClass, ContentStore, Effect, OutputLimits, SearchTool, Tool,
};
use serde_json::json;

use crate::{Workspace, failed, ok};

fn project(test: &str) -> Workspace {
    let ws = Workspace::new(test);
    ws.file(".gitignore", "target/\n");
    ws.file(".ignore", "vendor/\n");
    ws.file(
        "src/main.rs",
        "use demo::alpha;\n\nfn main() {\n    alpha();\n    println!(\"done\");\n}\n",
    );
    ws.file(
        "src/lib.rs",
        "/// Does the first thing.\npub fn alpha() {\n    beta();\n}\n\nfn beta() {}\n\n// ALPHA and omega\n",
    );
    ws.file("docs/guide.md", "# Guide\n\nCall `alpha` first.\n");
    ws.file("target/debug/build.rs", "fn alpha() {}\n");
    ws.file("vendor/dep.rs", "fn alpha() {}\n");
    ws.file(".hidden/notes.rs", "fn alpha() {}\n");
    ws.bytes("assets/blob.bin", b"alpha\0\x01\x02alpha\n");
    ws
}

// --- The arguments ---------------------------------------------------------

#[test]
fn matches_are_grouped_by_file_in_path_order_with_line_numbers() {
    let ws = project("search_lines");
    let result = ws.call(&SearchTool::new(), json!({"pattern": "alpha"}));
    assert_eq!(
        ok(&result),
        "4 matching lines in 3 files for `alpha` in `.`:\n\
         docs/guide.md\n  3: Call `alpha` first.\n\
         src/lib.rs\n  2: pub fn alpha() {\n\
         src/main.rs\n  1: use demo::alpha;\n  4:     alpha();"
    );
    assert_eq!(
        result.payload(),
        Some(&json!({"matches": 4, "files": 3, "shown": 4}))
    );
}

#[test]
fn the_result_is_the_same_every_time_whatever_the_threads_do() {
    let ws = Workspace::new("search_deterministic");
    for dir in 0..12 {
        for file in 0..12 {
            ws.file(
                &format!("d{dir:02}/f{file:02}.txt"),
                "needle here\nnothing\nneedle again\n",
            );
        }
    }
    let first = ok(&ws.call(&SearchTool::new().threads(8), json!({"pattern": "needle"})));
    for threads in [1, 2, 8, 8, 8] {
        let again = ok(&ws.call(
            &SearchTool::new().threads(threads),
            json!({"pattern": "needle"}),
        ));
        assert_eq!(again, first, "with {threads} threads");
    }
    assert!(first.starts_with(
        "288 matching lines in 144 files for `needle` in `.`. The first 100:\n\
         d00/f00.txt\n  1: needle here\n  3: needle again\nd00/f01.txt\n"
    ));
}

#[test]
fn the_pattern_is_a_regular_expression_matched_within_a_line() {
    let ws = project("search_regex");
    let tool = SearchTool::new();
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": r"^pub fn \w+\(\)"}))),
        "1 matching line in 1 file for `^pub fn \\w+\\(\\)` in `.`:\nsrc/lib.rs\n  2: pub fn alpha() {"
    );
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": r"\{\}$", "path": "src"}))),
        "1 matching line in 1 file for `\\{\\}$` in `src`:\nsrc/lib.rs\n  6: fn beta() {}"
    );
    // A pattern cannot reach across a line break.
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": r"alpha\(\) \{\s+beta"}))),
        "No matches for `alpha\\(\\) \\{\\s+beta` in `.`."
    );
}

#[test]
fn a_pattern_that_is_not_a_regular_expression_is_refused_with_the_reason() {
    let ws = project("search_bad_regex");
    let tool = SearchTool::new();
    let text = failed(&ws.call(&tool, json!({"pattern": "fn main("})));
    assert!(
        text.starts_with("`pattern` is not a regular expression this can search with:"),
        "{text}"
    );
    assert!(text.contains("unclosed group"), "{text}");
    assert!(text.contains("put a backslash before"), "{text}");
    // A line break in the pattern can never match one line.
    let text = failed(&ws.call(&tool, json!({"pattern": "a\nb"})));
    assert!(
        text.starts_with("`pattern` is not a regular expression this can search with:"),
        "{text}"
    );
    assert!(
        ws.plan(&tool, json!({"pattern": "("})).is_err(),
        "refused when planned"
    );
}

#[test]
fn ignore_case_matches_letters_whatever_their_case() {
    let ws = project("search_case");
    let text = ok(&ws.call(
        &SearchTool::new(),
        json!({"pattern": "alpha", "path": "src/lib.rs", "ignore_case": true}),
    ));
    assert_eq!(
        text,
        "2 matching lines in 1 file for `alpha` in `src/lib.rs`:\nsrc/lib.rs\n  2: pub fn alpha() {\n  8: // ALPHA and omega"
    );
}

#[test]
fn path_narrows_the_search_to_a_directory_or_one_file() {
    let ws = project("search_path");
    let tool = SearchTool::new();
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": "alpha", "path": "docs"}))),
        "1 matching line in 1 file for `alpha` in `docs`:\ndocs/guide.md\n  3: Call `alpha` first."
    );
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": "alpha", "path": "src/main.rs"}))),
        "2 matching lines in 1 file for `alpha` in `src/main.rs`:\nsrc/main.rs\n  1: use demo::alpha;\n  4:     alpha();"
    );
    // A file that is named is searched even where a walk would leave it out.
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": "alpha", "path": "vendor/dep.rs"}))),
        "1 matching line in 1 file for `alpha` in `vendor/dep.rs`:\nvendor/dep.rs\n  1: fn alpha() {}"
    );
    assert_eq!(
        failed(&ws.call(&tool, json!({"pattern": "alpha", "path": "nowhere"}))),
        "There is nothing at `nowhere` to search. `list` its parent to see what is there."
    );
}

#[test]
fn glob_keeps_only_files_that_match() {
    let ws = project("search_glob");
    let tool = SearchTool::new();
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": "alpha", "glob": "*.md"}))),
        "1 matching line in 1 file for `alpha` in `.` (files matching `*.md`):\n\
         docs/guide.md\n  3: Call `alpha` first."
    );
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"pattern": "alpha", "glob": "src/**/*.rs", "mode": "files"})
        )),
        "2 files with a match for `alpha` in `.` (files matching `src/**/*.rs`):\nsrc/lib.rs\nsrc/main.rs"
    );
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": "alpha", "glob": "*.py"}))),
        "No matches for `alpha` in `.` (files matching `*.py`)."
    );
}

#[test]
fn context_shows_the_lines_around_each_match() {
    let ws = project("search_context");
    let tool = SearchTool::new();
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"pattern": "beta", "path": "src/lib.rs", "context": 1})
        )),
        "2 matching lines in 1 file for `beta` in `src/lib.rs`:\nsrc/lib.rs\n\
         \x20 2- pub fn alpha() {\n\x20 3:     beta();\n\x20 4- }\n\x20 5- \n\x20 6: fn beta() {}\n\x20 7- "
    );
    // Groups that do not touch are separated.
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"pattern": "alpha|println", "path": "src/main.rs", "context": 0})
        )),
        "3 matching lines in 1 file for `alpha|println` in `src/main.rs`:\nsrc/main.rs\n\
         \x20 1: use demo::alpha;\n\x20 4:     alpha();\n\x20 5:     println!(\"done\");"
    );
    ws.file("gaps.txt", "hit one\na\nb\nc\nd\nhit two\n");
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"pattern": "hit", "path": "gaps.txt", "context": 1})
        )),
        "2 matching lines in 1 file for `hit` in `gaps.txt`:\ngaps.txt\n\
         \x20 1: hit one\n\x20 2- a\n\x20 --\n\x20 5- d\n\x20 6: hit two"
    );
    assert!(
        failed(&ws.call(&tool, json!({"pattern": "x", "context": 6})))
            .contains("`context` must be 5 or less, and is 6")
    );
}

#[test]
fn mode_files_names_the_files_and_mode_count_counts_per_file() {
    let ws = project("search_modes");
    let tool = SearchTool::new();
    let files = ws.call(&tool, json!({"pattern": "alpha", "mode": "files"}));
    assert_eq!(
        ok(&files),
        "3 files with a match for `alpha` in `.`:\ndocs/guide.md\nsrc/lib.rs\nsrc/main.rs"
    );
    assert_eq!(files.payload().unwrap()["files"], 3);
    assert_eq!(
        ok(&ws.call(&tool, json!({"pattern": "alpha", "mode": "count"}))),
        "4 matching lines in 3 files for `alpha` in `.`:\ndocs/guide.md: 1\nsrc/lib.rs: 1\nsrc/main.rs: 2"
    );
    assert!(
        failed(&ws.call(&tool, json!({"pattern": "alpha", "mode": "words"})))
            .contains("`mode` must be one of \"lines\", \"files\", \"count\"")
    );
}

#[test]
fn when_the_cap_is_hit_the_result_says_how_many_more_matches_exist() {
    let ws = Workspace::new("search_cap");
    for n in 0..10 {
        ws.file(&format!("f{n}.txt"), "hit\nmiss\nhit\nhit\n");
    }
    let tool = SearchTool::new();
    let result = ws.call(&tool, json!({"pattern": "hit", "max_results": 4}));
    assert_eq!(
        ok(&result),
        "30 matching lines in 10 files for `hit` in `.`. The first 4:\n\
         f0.txt\n  1: hit\n  3: hit\n  4: hit\nf1.txt\n  1: hit\n\
         [26 more matching lines not shown: `max_results` was reached. Narrow `pattern`, `path` \
         or `glob` to see them. `mode: \"count\"` shows where they are.]"
    );
    assert_eq!(
        result.payload(),
        Some(&json!({"matches": 30, "files": 10, "shown": 4}))
    );

    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"pattern": "hit", "mode": "files", "max_results": 2})
        )),
        "The first 2 of 10 files with a match for `hit` in `.`:\nf0.txt\nf1.txt\n\
         [8 more files not shown: `max_results` was reached. Narrow `pattern`, `path` or `glob` \
         to see them.]"
    );
    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"pattern": "hit", "mode": "count", "max_results": 2})
        )),
        "30 matching lines in 10 files for `hit` in `.`. The first 2 files:\nf0.txt: 3\nf1.txt: 3\n\
         [8 more files not shown: `max_results` was reached. Narrow `pattern`, `path` or `glob` \
         to see them.]"
    );
}

#[test]
fn the_default_cap_is_a_hundred_matches() {
    let ws = Workspace::new("search_default_cap");
    ws.file("many.txt", &"hit\n".repeat(250));
    let text = ok(&ws.call(&SearchTool::new(), json!({"pattern": "hit"})));
    assert!(
        text.starts_with(
            "250 matching lines in 1 file for `hit` in `.`. The first 100:\nmany.txt\n  1: hit\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "\n  100: hit\n[150 more matching lines not shown: `max_results` was reached."
        ),
        "{text}"
    );
}

#[test]
fn the_output_limits_cut_a_result_that_max_results_would_let_through() {
    let ws = Workspace::new("search_limits");
    ws.file("many.txt", &"hit\n".repeat(250));
    let cx = ws.cx().with_limits(OutputLimits::new(1 << 20, 20, 2000));
    let text = ok(&ws.call_with(&cx, &SearchTool::new(), json!({"pattern": "hit"})));
    assert!(
        text.starts_with(
            "250 matching lines in 1 file for `hit` in `.`. The first 11:\nmany.txt\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("[239 more matching lines not shown: the output limit was reached."),
        "{text}"
    );
}

#[test]
fn a_long_matching_line_is_shortened_with_a_marker() {
    let ws = Workspace::new("search_long_line");
    ws.file(
        "min.js",
        &format!("var a=1;{}needle{}\n", "x".repeat(5000), "y".repeat(5000)),
    );
    let text = ok(&ws.call(&SearchTool::new(), json!({"pattern": "needle"})));
    let line = text.lines().nth(2).unwrap();
    assert!(line.starts_with("  1: var a=1;xxxx"));
    // The marker counts what is missing of the whole line, not of the part
    // the search kept while it ran.
    let (kept, marker) = line.rsplit_once("… [+").expect("a marker");
    let missing: usize = marker
        .trim_end_matches(" characters]")
        .replace(',', "")
        .parse()
        .expect("a number");
    assert_eq!(
        kept.chars().count() + missing,
        "  1: ".len() + 8 + 5000 + 6 + 5000
    );
    assert!(
        line.ends_with("characters]"),
        "{}",
        &line[line.len() - 30..]
    );
    assert!(line.chars().count() <= 2000);
}

// --- What is searched ------------------------------------------------------

#[test]
fn ignored_hidden_and_binary_files_are_not_searched() {
    let ws = project("search_ignored");
    // `target/` is in .gitignore, `vendor/` in .ignore, `.hidden/` is
    // hidden and `assets/blob.bin` has a NUL byte: all four have a match.
    let text = ok(&ws.call(
        &SearchTool::new(),
        json!({"pattern": "alpha", "mode": "files"}),
    ));
    assert_eq!(
        text,
        "3 files with a match for `alpha` in `.`:\ndocs/guide.md\nsrc/lib.rs\nsrc/main.rs"
    );
    assert_eq!(
        failed(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "alpha", "path": "assets/blob.bin"})
        )),
        "`assets/blob.bin` is a binary file, so it is not searched."
    );
}

#[test]
fn a_gitignore_is_honoured_without_a_git_repository_and_in_subdirectories() {
    let ws = Workspace::new("search_nested_ignore");
    ws.file("pkg/.gitignore", "dist/\n!dist/keep.txt\nsecret-*.txt\n");
    ws.file("pkg/dist/out.txt", "needle\n");
    ws.file("pkg/secret-1.txt", "needle\n");
    ws.file("pkg/src.txt", "needle\n");
    ws.file("dist/top.txt", "needle\n");
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "mode": "files"})
        )),
        "2 files with a match for `needle` in `.`:\ndist/top.txt\npkg/src.txt"
    );
    // Searching below the ignore file still honours it.
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "mode": "files", "path": "pkg"})
        )),
        "1 file with a match for `needle` in `pkg`:\npkg/src.txt"
    );
}

#[test]
fn both_line_endings_end_a_line() {
    let ws = Workspace::new("search_crlf");
    ws.bytes("win.txt", b"first line\r\nthe end\r\nlast\r\n");
    assert_eq!(
        ok(&ws.call(&SearchTool::new(), json!({"pattern": "end$"}))),
        "1 matching line in 1 file for `end$` in `.`:\nwin.txt\n  2: the end"
    );
}

#[test]
fn nothing_found_is_a_result_not_an_error() {
    let ws = project("search_nothing");
    let result = ws.call(&SearchTool::new(), json!({"pattern": "zebra"}));
    assert_eq!(ok(&result), "No matches for `zebra` in `.`.");
    assert_eq!(
        result.payload(),
        Some(&json!({"matches": 0, "files": 0, "shown": 0}))
    );
}

// --- Artifacts --------------------------------------------------------------

#[test]
fn an_artifact_is_searched_by_its_id() {
    let ws = Workspace::new("search_artifact");
    let mut output = String::new();
    for n in 1..=5000 {
        output.push_str(&if n == 3210 {
            "error: the build failed\n".to_owned()
        } else {
            format!("compiling unit {n}\n")
        });
    }
    let artifact = ws
        .store
        .put_artifact("full output of shell", "text/plain", output.as_bytes())
        .unwrap();
    let tool = SearchTool::new();
    let id = artifact.id.as_str();

    let plan = ws
        .plan(&tool, json!({"pattern": "error", "artifact": id}))
        .unwrap();
    assert_eq!(plan.summary, format!("Search artifact {id} for error"));
    assert!(plan.effects.is_empty());

    assert_eq!(
        ok(&ws.call(
            &tool,
            json!({"pattern": "^error", "artifact": id, "context": 1})
        )),
        format!(
            "1 matching line in 1 file for `^error` in `artifact {id}`:\nartifact {id}\n\
             \x20 3209- compiling unit 3209\n\x20 3210: error: the build failed\n\x20 3211- compiling unit 3211"
        )
    );
    assert_eq!(
        failed(&ws.call(&tool, json!({"pattern": "x", "artifact": "a_9999"}))),
        "There is no artifact `a_9999` in this session. An artifact id is given in the notice \
         at the end of a result that was cut."
    );
    assert!(
        failed(&ws.call(
            &tool,
            json!({"pattern": "x", "artifact": id, "path": "src"})
        ))
        .contains("`search` takes `path` or `artifact`, not both")
    );
}

// --- Effects, scheduling, cancellation -------------------------------------

#[test]
fn a_search_of_a_directory_is_a_walk_of_its_tree_and_of_a_file_a_read() {
    let ws = project("search_plan");
    let tool = SearchTool::new();
    assert_eq!(tool.spec().name, "search");
    assert_eq!(tool.spec().concurrency, ConcurrencyClass::ReadOnly);

    let plan = ws.plan(&tool, json!({"pattern": "alpha"})).unwrap();
    assert_eq!(plan.summary, "Search . for alpha");
    assert_eq!(plan.concurrency, Concurrency::ReadOnly);
    assert!(!plan.has_side_effects());
    assert!(matches!(plan.effects.as_slice(), [Effect::ReadTree(path)] if path.display() == "."));

    let plan = ws
        .plan(&tool, json!({"pattern": "alpha", "path": "src"}))
        .unwrap();
    assert!(matches!(plan.effects.as_slice(), [Effect::ReadTree(path)] if path.display() == "src"));
    let plan = ws
        .plan(&tool, json!({"pattern": "alpha", "path": "src/lib.rs"}))
        .unwrap();
    assert!(
        matches!(plan.effects.as_slice(), [Effect::Read(path)] if path.display() == "src/lib.rs")
    );
}

#[test]
fn a_search_over_a_generated_tree_finds_every_match_and_can_be_cancelled() {
    // A tree large enough to keep several threads busy, and small enough
    // for the blocking tier: 40 directories of 25 files. The 100,000-file
    // measurement is `perf_search_of_a_large_tree`.
    let ws = Workspace::new("search_tree");
    for dir in 0..40 {
        for file in 0..25 {
            let needle = if file % 5 == 0 {
                "the needle is here\n"
            } else {
                ""
            };
            ws.file(
                &format!("d{dir:02}/sub/f{file:02}.txt"),
                &format!("first line\n{needle}last line of {dir}/{file}\n"),
            );
        }
    }
    let tool = SearchTool::new();
    let result = ws.call(
        &tool,
        json!({"pattern": "needle", "mode": "count", "max_results": 2000}),
    );
    let text = ok(&result);
    assert!(text.starts_with("200 matching lines in 200 files for `needle` in `.`:\nd00/sub/f00.txt: 1\nd00/sub/f05.txt: 1\n"), "{text}");
    assert_eq!(text.lines().count(), 201);
    assert_eq!(
        result.payload(),
        Some(&json!({"matches": 200, "files": 200, "shown": 200}))
    );

    // A token that has fired: the search does not start.
    let cx = ws.cx();
    cx.cancel.cancel();
    let cancelled = ws.call_with(&cx, &tool, json!({"pattern": "needle"}));
    assert_eq!(cancelled.outcome(), &ToolOutcome::Interrupted);
    assert_eq!(
        cancelled.text(),
        "`search` was interrupted before it finished."
    );
}

#[test]
fn an_ignore_file_with_windows_line_endings_is_read_the_same() {
    let ws = Workspace::new("search_ignore_crlf");
    ws.file(".gitignore", "target/\r\n*.log\r\n");
    ws.file("target/out.txt", "needle\n");
    ws.file("build.log", "needle\n");
    ws.file("kept.txt", "needle\n");
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "mode": "files"})
        )),
        "1 file with a match for `needle` in `.`:\nkept.txt"
    );
}
