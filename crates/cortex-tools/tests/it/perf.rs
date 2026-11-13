//! The search budget of `docs/performance-budgets.md`: a literal searched
//! for in a tree of 100,000 files in at most a second on a desktop.
//!
//! This is a measurement, not a blocking test: making the tree takes longer
//! than a test may. It is ignored by default and run by hand, in a release
//! build, outside nextest (whose limit on one test would stop it):
//!
//! ```text
//! cargo test --release -p cortex-tools --test it perf_ -- --ignored --nocapture
//! ```
//!
//! `CORTEX_PERF_FILES` sets the number of files (100,000 unless given), and
//! `CORTEX_PERF_THREADS` the number of walker threads (the walker's own
//! choice unless given). The numbers it prints go into the page above.

use std::time::Instant;

use cortex_tools::{SearchTool, Tool};
use serde_json::json;

use crate::{Workspace, ok};

fn number_from_env(variable: &str, default: usize) -> usize {
    std::env::var(variable)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

/// One run of a search, in milliseconds, with its result's first line.
fn timed(ws: &Workspace, tool: &dyn Tool, arguments: serde_json::Value) -> (f64, String) {
    let start = Instant::now();
    let result = ws.call(tool, arguments);
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let text = ok(&result);
    (elapsed, text.lines().next().unwrap_or("").to_owned())
}

#[test]
#[ignore = "a measurement, run by hand in a release build: see the module documentation"]
fn perf_search_of_a_large_tree() {
    let files = number_from_env("CORTEX_PERF_FILES", 100_000);
    let threads = number_from_env("CORTEX_PERF_THREADS", 0);
    let ws = Workspace::new("perf_search_large_tree");

    // A tree shaped like a large repository: a thousand files to a
    // directory at most, two levels down, a dozen short lines to a file, and
    // the literal in one file in a hundred.
    let start = Instant::now();
    let mut with_literal = 0;
    for file in 0..files {
        let directory = format!("pkg{:03}/mod{:02}", file / 1000, (file / 100) % 10);
        let mut content = String::with_capacity(512);
        for line in 0..12 {
            content.push_str(&format!(
                "pub fn function_{file}_{line}(input: &str) -> usize {{ input.len() + {line} }}\n"
            ));
        }
        if file % 100 == 7 {
            content.push_str("// the_literal_we_look_for is on this line\n");
            with_literal += 1;
        }
        ws.file(&format!("{directory}/file_{file:06}.rs"), &content);
    }
    println!(
        "made {files} files ({with_literal} with the literal) in {:.1} s",
        start.elapsed().as_secs_f64()
    );

    let tool = SearchTool::new().threads(threads);
    let literal =
        json!({"pattern": "the_literal_we_look_for", "mode": "count", "max_results": 2000});
    let mut runs = Vec::new();
    for run in 0..7 {
        let (elapsed, header) = timed(&ws, &tool, literal.clone());
        assert!(
            header.starts_with(&format!(
                "{} matching lines in {} files",
                cortex_tools_count(with_literal),
                cortex_tools_count(with_literal)
            )),
            "{header}"
        );
        println!("literal, run {run}: {elapsed:.0} ms");
        runs.push(elapsed);
    }
    runs.sort_by(f64::total_cmp);
    println!(
        "literal over {files} files, {} threads: best {:.0} ms, median {:.0} ms, worst {:.0} ms",
        if threads == 0 {
            "default".to_owned()
        } else {
            threads.to_string()
        },
        runs[0],
        runs[runs.len() / 2],
        runs[runs.len() - 1]
    );

    // The same tree, three other ways: a pattern that matches every line, a
    // regular expression with no literal in it, and a miss.
    for (name, arguments) in [
        ("every line (pub fn)", json!({"pattern": "pub fn"})),
        (
            "regex (function_\\d+7_3\\b)",
            json!({"pattern": r"function_\d+7_3\b", "mode": "files"}),
        ),
        ("no match", json!({"pattern": "no_such_text_anywhere"})),
        (
            "glob *.md, no file matches",
            json!({"pattern": "pub", "glob": "*.md"}),
        ),
    ] {
        let (elapsed, header) = timed(&ws, &tool, arguments);
        println!("{name}: {elapsed:.0} ms  ({header})");
    }
}

/// A number with a comma between its thousands, as results write them.
fn cortex_tools_count(number: usize) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}
