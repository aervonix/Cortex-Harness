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

/// The budget "starting a shell command, harness overhead" of
/// `docs/performance-budgets.md`: what the harness adds to a command that
/// does nothing, over starting the same shell with the standard library.
///
/// ```text
/// cargo test --release -p cortex-tools --test it perf_ -- --ignored --nocapture
/// ```
///
/// `CORTEX_PERF_RUNS` sets the number of runs of each case (200 unless
/// given).
#[test]
#[ignore = "a measurement, run by hand in a release build: see the module documentation"]
fn perf_starting_a_command() {
    use std::sync::Arc;

    use cortex_tools::process::{Direct, LaunchCx, Launcher, Runner, Start};
    use cortex_tools::{MemoryStore, NoOutput, ShellConfig, ShellSearch, ShellTool, execute};

    let runs = number_from_env("CORTEX_PERF_RUNS", 200);
    let ws = Workspace::new("perf_start");
    let mut shell = ShellSearch::from_env().find(None, None).expect("a shell");
    let probe = Instant::now();
    shell.probe_version(std::time::Duration::from_secs(30));
    println!(
        "the shell: {}; asking its version took {:.1} ms",
        shell.name(),
        probe.elapsed().as_secs_f64() * 1000.0
    );
    let command = if shell.kind() == cortex_tools::ShellKind::Cmd {
        "ver >NUL"
    } else {
        "exit 0"
    };
    let line = shell.command_line(command).expect("runnable");
    let env: Vec<_> = std::env::vars_os().collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime");
    let measure = |name: &str, one: &mut dyn FnMut()| {
        for _ in 0..10 {
            one();
        }
        let mut times: Vec<f64> = (0..runs)
            .map(|_| {
                let start = Instant::now();
                one();
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!(
            "{name:<62} median {:>7.3} ms   p95 {:>7.3} ms",
            times[times.len() / 2],
            times[times.len() * 95 / 100]
        );
        times[times.len() / 2]
    };

    // The floor: the same command line, started and waited for with the
    // standard library, with nothing read.
    let floor = measure("the shell alone, standard library", &mut || {
        let mut plain = std::process::Command::new(&line.program);
        for argument in &line.arguments {
            match argument {
                cortex_tools::process::Argument::Exact(text) => {
                    plain.arg(text);
                }
                cortex_tools::process::Argument::Verbatim(text) => {
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        plain.raw_arg(text);
                    }
                    #[cfg(not(windows))]
                    plain.arg(text);
                }
            }
        }
        plain
            .current_dir(ws.root())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        plain.spawn().expect("start").wait().expect("wait");
    });

    let cx = ws.cx();
    let launch = Direct
        .launch(
            line.clone(),
            &LaunchCx::new(cx.paths.root(), cx.paths.root()),
        )
        .expect("a launch");
    let start = || Start {
        launch: launch.clone(),
        cwd: ws.root(),
        env: env.clone(),
        timeout: std::time::Duration::from_secs(60),
        label: "perf".to_owned(),
    };
    let store: Arc<dyn cortex_tools::ContentStore> = Arc::new(MemoryStore::new());
    let cancel = cortex_base::CancelToken::new();
    let bare = Runner::new();
    let runner = measure(
        "the runner: a process tree, a pipe, two threads",
        &mut || {
            runtime
                .block_on(bare.run(start(), &cancel, Arc::clone(&store), &mut NoOutput))
                .expect("run");
        },
    );
    let guarded = Runner::new().with_guard(crate::guard());
    let with_guard = measure("the runner, with the guard told of the tree", &mut || {
        runtime
            .block_on(guarded.run(start(), &cancel, Arc::clone(&store), &mut NoOutput))
            .expect("run");
    });

    let quiet = ShellTool::new(
        shell.clone(),
        ShellConfig {
            detect_changes: false,
            ..ShellConfig::default()
        },
    )
    .with_runner(Runner::new().with_guard(crate::guard()));
    let call = json!({ "command": command });
    let tool = measure(
        "the tool: plan, run, result; no look for changes",
        &mut || {
            let plan = quiet.plan(&call, &cx.plan_cx()).expect("a plan");
            let result = runtime.block_on(execute(&quiet, plan, &cx, &mut NoOutput));
            assert!(!result.is_error(), "{}", result.text());
        },
    );
    // The look for changes, without git: one walk of the workspace after
    // the command. Then with it: two answers of `git status`.
    let files = number_from_env("CORTEX_PERF_FILES", 500);
    for file in 0..files {
        ws.file(
            &format!("src/dir{:02}/file{file:04}.rs", file % 20),
            "pub fn one() -> u32 { 1 }\n",
        );
    }
    let looking = ShellTool::new(shell.clone(), ShellConfig::default())
        .with_runner(Runner::new().with_guard(crate::guard()));
    let look = |name: &str, tool: &ShellTool| {
        let mut method = String::new();
        let median = measure(name, &mut || {
            let plan = tool.plan(&call, &cx.plan_cx()).expect("a plan");
            let result = runtime.block_on(execute(tool, plan, &cx, &mut NoOutput));
            assert!(!result.is_error(), "{}", result.text());
            method = result.payload().expect("a payload")["changes"]["method"].to_string();
        });
        println!("    (the changes were looked for by {method})");
        median
    };
    // Inside a checkout of this repository the first of the two is git's
    // answer for this repository; on a copy without `.git` it is the walk.
    let with_walk = look(
        &format!("the tool, looking for changes, {files} files, no repository"),
        &looking,
    );
    crate::changes::repository(&ws, &[]);
    let looking = ShellTool::new(shell, ShellConfig::default())
        .with_runner(Runner::new().with_guard(crate::guard()));
    let with_changes = look(
        &format!("the tool, looking for changes, {files} files, a repository"),
        &looking,
    );
    println!(
        "the look for changes adds {:.3} ms without a repository",
        with_walk - tool
    );
    println!(
        "overhead over the shell alone: runner {:.3} ms, with the guard {:.3} ms, the tool \
         {:.3} ms, the tool with the look for changes {:.3} ms",
        runner - floor,
        with_guard - floor,
        tool - floor,
        with_changes - floor
    );
}
