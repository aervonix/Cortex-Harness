//! The `shell` tool with each shell: quoting, merged output, exit status,
//! the environment, the working directory, and output too large to show
//! (`docs/architecture/05-tools.md`, "`shell`").
//!
//! Every test here that runs a command is a system test, and runs once for
//! each shell of the system it is on: `bash` and `sh` on Linux, PowerShell
//! 7, Windows PowerShell and `cmd.exe` on Windows. What differs between
//! shells is the command a test gives, never what it expects back.

use std::sync::Arc;

use cortex_protocol::item::ToolOutcome;
use cortex_testkit::children::{ChildSpec, Roll, echo_arguments, echoed};
use cortex_tools::shell::{ShellConfig, Unquotable};
use cortex_tools::{
    Concurrency, ConcurrencyClass, Effect, Shell, ShellKind, ShellTool, Tool, ToolRegistry, Toolbox,
};
use serde_json::json;

use crate::bench::{self, Bench, count_lines, exit_with, invoke, list_names, print_variable};
use crate::{ENTRY, Workspace};

/// Texts that a careless quoting would let become something else: a second
/// command, an expansion, a redirection, a lost character.
const HOSTILE: &[&str] = &[
    "plain",
    "two words",
    "",
    "it's",
    "say \"hi\"",
    "a&b",
    "a && echo injected",
    "a|b",
    "| echo injected",
    "a^b",
    "%PATH%",
    "100%",
    "$HOME",
    "$(echo injected)",
    "`echo injected`",
    ";echo injected",
    "two\nlines",
    "C:\\dir\\",
    "back\\\\slash\\\"quote",
    "*",
    "~",
    "!x!",
    "\u{2018}typographic\u{2019}",
    "<in >out",
    "(paren)",
    "tab\there",
    "caf\u{e9} \u{65e5}\u{672c}",
];

fn is_powershell(shell: &Shell) -> bool {
    shell.kind().is_powershell()
}

/// The lines of a result's text, without the spaces a shell may leave at
/// the end of one.
fn lines(text: &str) -> Vec<&str> {
    text.lines().map(str::trim_end).collect()
}

fn output_is_merged_in_order(test: &str, shell: Shell) {
    let bench = Bench::new(test, shell);
    let command = match bench.shell().kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            "[Console]::Out.WriteLine('out 1'); [Console]::Error.WriteLine('err 1'); \
             [Console]::Out.WriteLine('out 2'); [Console]::Error.WriteLine('err 2')"
        }
        ShellKind::Cmd => "echo out 1& echo err 1 1>&2& echo out 2& echo err 2 1>&2",
        _ => "echo out 1; echo err 1 >&2; echo out 2; echo err 2 >&2",
    };
    let ran = bench.command(command);
    assert_eq!(
        ran.result.outcome(),
        &ToolOutcome::Success,
        "{}",
        ran.text()
    );
    let text = ran.text();
    let shown = lines(&text);
    assert_eq!(shown[..4], ["out 1", "err 1", "out 2", "err 2"], "{text}");
    assert!(shown[4].starts_with("[exit code 0, after "), "{text}");
    assert_eq!(shown.len(), 5, "{text}");
    // What was streamed while it ran is the same output.
    let streamed = ran.streamed.replace("\r\n", "\n");
    assert_eq!(lines(&streamed), ["out 1", "err 1", "out 2", "err 2"]);
    assert_eq!(ran.payload("exit_code"), json!(0));
    assert_eq!(ran.payload("shell"), json!(bench.shell().name()));
}

fn the_exit_status_is_the_commands(test: &str, shell: Shell) {
    let bench = Bench::new(test, shell);
    let roll = Roll::new();

    // The shell's own exit.
    let ran = bench.command(&exit_with(bench.shell(), 3));
    assert_eq!(ran.result.outcome(), &ToolOutcome::Error);
    assert_eq!(ran.payload("exit_code"), json!(3));
    assert!(
        ran.text().starts_with("(no output)\n[exit code 3, after "),
        "{}",
        ran.text()
    );

    // A program's exit code, when the program is the last thing run.
    let failing = bench::child(bench.shell(), &ChildSpec::new(&roll, "failing").exiting(5));
    let ran = bench.command(&failing);
    assert_eq!(ran.payload("exit_code"), json!(5), "{}", ran.text());
    assert_eq!(ran.result.outcome(), &ToolOutcome::Error);

    // And the last thing run decides: a failure before a success is a
    // success.
    let separator = if bench.shell().kind() == ShellKind::Cmd {
        "&"
    } else {
        ";"
    };
    let ran = bench.command(&format!("{failing} {separator} echo fine"));
    assert_eq!(ran.payload("exit_code"), json!(0), "{}", ran.text());
    assert_eq!(ran.result.outcome(), &ToolOutcome::Success);
    assert!(ran.text().contains("fine"), "{}", ran.text());

    if is_powershell(bench.shell()) {
        // A cmdlet that fails has no exit code of its own: 1.
        let ran = bench.command("Get-Item 'no-such-file-for-this-test'");
        assert_eq!(ran.payload("exit_code"), json!(1), "{}", ran.text());
        assert!(
            ran.text().contains("no-such-file-for-this-test"),
            "{}",
            ran.text()
        );
        // A script that cannot be read at all fails too.
        let ran = bench.command("if (");
        assert_ne!(ran.payload("exit_code"), json!(0), "{}", ran.text());
    }
    if cfg!(unix) {
        // An exit by signal is reported as one.
        let ran = bench.command("kill -9 $$");
        assert_eq!(ran.payload("signal"), json!(9), "{}", ran.text());
        assert_eq!(ran.payload("exit_code"), json!(null));
        assert!(
            ran.text().contains("[killed by signal 9 (SIGKILL), after "),
            "{}",
            ran.text()
        );
        assert_eq!(ran.result.outcome(), &ToolOutcome::Error);
    }
}

fn hostile_arguments_arrive_unchanged(test: &str, shell: Shell) {
    let bench = Bench::new(test, shell);
    let shell = bench.shell().clone();
    let (program, prefix) = echo_arguments(ENTRY);

    if is_powershell(&shell) {
        // Through PowerShell itself: every text, as one line of JSON.
        let quoted: Vec<String> = HOSTILE
            .iter()
            .map(|text| shell.quote(text).expect("PowerShell quotes any text"))
            .collect();
        let ran = bench.command(&format!("ConvertTo-Json -Compress @({})", quoted.join(",")));
        assert_eq!(
            ran.result.outcome(),
            &ToolOutcome::Success,
            "{}",
            ran.text()
        );
        let text = ran.text();
        let line = text
            .lines()
            .find(|line| line.starts_with('['))
            .unwrap_or_else(|| panic!("no JSON in {text}"));
        let arrived: Vec<String> = serde_json::from_str(line).expect("a list of strings");
        assert_eq!(arrived, HOSTILE);
    }

    // To a program. Windows PowerShell is known to lose a double quote and
    // an empty text on the way to a program, whatever the quoting, so for
    // the PowerShells those are left to the test above.
    let mut expected = Vec::new();
    let mut words = Vec::new();
    for text in HOSTILE {
        if is_powershell(&shell) && (text.is_empty() || text.contains('"') || text.ends_with('\\'))
        {
            continue;
        }
        match shell.quote(text) {
            Ok(quoted) => {
                words.push(quoted);
                expected.push((*text).to_owned());
            }
            Err(refusal) => {
                // Only cmd.exe refuses, and only what it cannot carry.
                assert_eq!(shell.kind(), ShellKind::Cmd, "{text:?}: {refusal}");
                assert!(matches!(refusal, Unquotable::NotForCmd(_)));
                assert!(text.contains(['"', '%', '\n', '\r']), "{text:?}");
            }
        }
    }
    let command = format!("{} {}", invoke(&shell, &program, &prefix), words.join(" "));
    let ran = bench.command(&command);
    assert_eq!(
        ran.result.outcome(),
        &ToolOutcome::Success,
        "{}",
        ran.text()
    );
    let arrived = echoed(&ran.result.text());
    assert_eq!(arrived, [expected], "{}", ran.text());
    // Nothing in any of them ran.
    assert!(
        !lines(&ran.text()).contains(&"injected"),
        "a quoted text was run:\n{}",
        ran.text()
    );
}

fn the_command_reaches_the_shell_as_written(test: &str, shell: Shell) {
    let bench = Bench::new(test, shell);
    match bench.shell().kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            // Quotes after backslashes, and backslashes at the end: what
            // the command line's own quoting has to get exactly right.
            let ran = bench.command("[Console]::Out.WriteLine('x\\\"y\\\\')\n'second line'");
            let text = ran.text();
            assert_eq!(lines(&text)[..2], ["x\\\"y\\\\", "second line"], "{text}");
            // An exit of the command's own is not swallowed by what the
            // harness puts after it.
            let ran = bench.command("'before'; exit 7; 'after'");
            assert_eq!(ran.payload("exit_code"), json!(7));
            assert!(!ran.text().contains("after\n"), "{}", ran.text());
            // A back-tick at the end joins the next line to the command.
            // The next line is empty, so the line that sets the exit code
            // stays a line of its own: the command succeeds, and nothing
            // of the harness's is printed.
            let ran = bench.command("Write-Output 'kept' `");
            let text = ran.text();
            assert_eq!(lines(&text)[0], "kept", "{text}");
            assert_eq!(ran.payload("exit_code"), json!(0), "{text}");
            assert!(
                !text.contains("LASTEXITCODE") && !text.contains("exit 0"),
                "{text}"
            );
            // A failure before a trailing back-tick is still a failure.
            let ran = bench.command("Get-Item 'no-such-file-for-this-test' `");
            assert_eq!(ran.payload("exit_code"), json!(1), "{}", ran.text());
            // A command that runs on into the harness's last line some
            // other way does not parse, and then nothing of it runs.
            let ran = bench.command("Write-Output 'not printed'; Write-Output \"open");
            assert_ne!(ran.payload("exit_code"), json!(0), "{}", ran.text());
            assert!(
                !lines(&ran.text()).contains(&"not printed"),
                "{}",
                ran.text()
            );
        }
        ShellKind::Cmd => {
            // Inside quotes the operators are text; outside they work.
            let ran = bench.command("echo \"a & b | c ^ d\" & echo second");
            let text = ran.text();
            assert_eq!(lines(&text)[..2], ["\"a & b | c ^ d\"", "second"], "{text}");
            // An odd number of quotes is the command's own business too.
            let ran = bench.command("echo it\"s & echo unreached");
            assert_eq!(lines(&ran.text())[0], "it\"s & echo unreached");
            // A variable that is not set stays as it was written.
            let ran = bench.command("echo %CORTEX_NO_SUCH_VARIABLE% 100%");
            assert_eq!(lines(&ran.text())[0], "%CORTEX_NO_SUCH_VARIABLE% 100%");
        }
        _ => {
            let ran = bench.command(
                "printf '%s\\n' \"a\\\"b\" 'c\\d' \"$((1+2))\" 'it'\\''s'\necho 'second line'",
            );
            let text = ran.text();
            assert_eq!(
                lines(&text)[..5],
                ["a\"b", "c\\d", "3", "it's", "second line"],
                "{text}"
            );
        }
    }
}

fn the_environment_the_directory_and_the_input(test: &str, shell: Shell) {
    let bench = Bench::new(test, shell);
    let shell = bench.shell().clone();

    // Tools are told not to draw and not to ask.
    let separator = if shell.kind() == ShellKind::Cmd {
        "&"
    } else {
        ";"
    };
    let prints: Vec<String> = ["TERM", "NO_COLOR", "GIT_PAGER", "GIT_TERMINAL_PROMPT"]
        .iter()
        .map(|name| print_variable(&shell, name))
        .collect();
    let ran = bench.command(&prints.join(separator));
    assert_eq!(
        lines(&ran.text())[..4],
        ["dumb", "1", "cat", "0"],
        "{}",
        ran.text()
    );

    // The working directory is the workspace root, or `cwd`, whatever
    // characters its name has: it is never part of a command line.
    bench.ws.file("sub & dir/in-the-sub-directory.txt", "");
    bench.ws.file("at-the-root.txt", "");
    let ran = bench.command(list_names(&shell));
    let text = ran.text();
    assert!(lines(&text).contains(&"at-the-root.txt"), "{text}");
    assert!(!text.contains("in-the-sub-directory.txt"), "{text}");
    let ran = bench.run(json!({ "command": list_names(&shell), "cwd": "sub & dir" }));
    assert_eq!(
        lines(&ran.text())[0],
        "in-the-sub-directory.txt",
        "{}",
        ran.text()
    );

    // The input is closed: a command that reads it gets its end at once,
    // and does not wait for a key nobody can press.
    let reads = match shell.kind() {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            "$read = [Console]::In.ReadToEnd(); \"read $($read.Length) characters\""
        }
        ShellKind::Cmd => "findstr x & echo read 0 characters",
        _ => "wc -c | tr -d ' ' | sed 's/.*/read & characters/'",
    };
    let ran = bench.command(reads);
    assert_eq!(lines(&ran.text())[0], "read 0 characters", "{}", ran.text());
}

fn output_too_large_to_show(test: &str, shell: Shell) {
    let bench = Bench::new(test, shell);
    const LINES: usize = 6000;
    let ran = bench.command(&count_lines(bench.shell(), LINES));
    assert_eq!(
        ran.result.outcome(),
        &ToolOutcome::Success,
        "{}",
        ran.text()
    );
    let whole: String = (1..=LINES)
        .map(|number| format!("line {number}\n"))
        .collect();
    let text = ran.text();

    // The beginning and the end, with markers for what is between them.
    // Lines that differ only in a number are folded to a count as well,
    // which is the output policy's doing and is tested with it.
    assert!(text.starts_with("line 1\n"), "{text}");
    assert!(
        text.contains(&format!("line {LINES}\n[Output cut: ")),
        "{text}"
    );
    assert!(text.contains(" lines not shown …]\n"), "{text}");
    assert!(
        text.lines()
            .last()
            .unwrap()
            .starts_with("[exit code 0, after ")
    );

    // All of it is in the artifact, which the notice names.
    let artifact = ran.result.artifacts().first().expect("the output was cut");
    assert!(
        text.contains(&format!("is artifact {}", artifact.id)),
        "{text}"
    );
    let stored = bench.ws.store.artifact(&artifact.id).expect("stored");
    let stored_text = String::from_utf8(stored.clone())
        .expect("text")
        .replace("\r\n", "\n");
    assert_eq!(stored_text, whole);
    assert_eq!(ran.payload("output_bytes"), json!(stored.len()));
    assert_eq!(
        bench.ws.store.artifact_info(&artifact.id),
        Some(("full output of shell".to_owned(), "text/plain".to_owned()))
    );
    // The notice says how large all of it was.
    assert!(
        text.contains(&format!(
            "The whole output ({}) is artifact",
            size(stored.len())
        )),
        "{text}"
    );

    // It was streamed as it arrived, from its first line.
    let streamed = ran.streamed.replace("\r\n", "\n");
    assert!(streamed.starts_with("line 1\nline 2\n"));
    assert!(streamed.contains(&format!("line {LINES}\n")));
}

/// A size as the output policy writes one, for the sizes of this test.
fn size(bytes: usize) -> String {
    let tenths = (bytes * 10 + 512) / 1024;
    format!("{}.{} KiB", tenths / 10, tenths % 10)
}

fn the_model_is_told_the_shell(test: &str, mut shell: Shell) {
    // The version is asked of the shell itself.
    shell.probe_version(crate::LIMIT);
    match shell.kind() {
        ShellKind::Bash | ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            let version = shell.version().expect("this shell says its version");
            assert!(
                version.starts_with(|c: char| c.is_ascii_digit()),
                "{version}"
            );
            assert!(version.contains('.'), "{version}");
        }
        _ => assert_eq!(shell.version(), None),
    }
    let bench = Bench::new(test, shell);
    let name = bench.shell().name();
    let description = &bench.tool.spec().description;
    assert!(
        description.contains(&format!("Run a command in {name} on ")),
        "{description}"
    );
    println!("the shell: {name}");
}

/// One module for each shell, with every test of this file in it.
macro_rules! for_shell {
    ($module:ident, $program:literal) => {
        mod $module {
            use super::*;

            fn run(test: &str, body: fn(&str, Shell)) {
                if let Some(shell) = bench::shell($program) {
                    body(&format!("{}_{test}", stringify!($module)), shell);
                }
            }

            #[test]
            fn sys_output_is_merged_in_order() {
                run("merged", output_is_merged_in_order);
            }

            #[test]
            fn sys_the_exit_status_is_the_commands() {
                run("exit", the_exit_status_is_the_commands);
            }

            #[test]
            fn sys_hostile_arguments_arrive_unchanged() {
                run("hostile", hostile_arguments_arrive_unchanged);
            }

            #[test]
            fn sys_the_command_reaches_the_shell_as_written() {
                run("as_written", the_command_reaches_the_shell_as_written);
            }

            #[test]
            fn sys_the_environment_the_directory_and_the_input() {
                run("surroundings", the_environment_the_directory_and_the_input);
            }

            #[test]
            fn sys_output_too_large_to_show() {
                run("large", output_too_large_to_show);
            }

            #[test]
            fn sys_the_model_is_told_the_shell() {
                run("told", the_model_is_told_the_shell);
            }

            #[test]
            fn sys_a_background_process_is_stopped_and_said_so() {
                run(
                    "background",
                    crate::process::a_background_process_is_stopped,
                );
            }
        }
    };
}

#[cfg(unix)]
for_shell!(bash, "bash");
#[cfg(unix)]
for_shell!(sh, "sh");
#[cfg(windows)]
for_shell!(pwsh, "pwsh");
#[cfg(windows)]
for_shell!(powershell, "powershell");
#[cfg(windows)]
for_shell!(cmd, "cmd");

/// Git Bash, where the machine has it: bash on Windows is spoken to as
/// bash anywhere.
#[cfg(windows)]
#[test]
fn sys_git_bash_is_bash() {
    let candidates = [
        "C:\\Program Files\\Git\\bin\\bash.exe",
        "C:\\Program Files\\Git\\usr\\bin\\bash.exe",
    ];
    let Some(program) = candidates
        .iter()
        .find(|path| std::path::Path::new(path).is_file())
    else {
        println!("skipped: Git Bash is not installed here");
        return;
    };
    let shell = cortex_tools::ShellSearch::from_env()
        .find(Some(program), None)
        .expect("the file is there");
    assert_eq!(shell.kind(), ShellKind::Bash);
    output_is_merged_in_order("git_bash_merged", shell.clone());
    let bench = Bench::new("git_bash_exit", shell);
    let ran = bench.command("printf '%s\\n' 'it'\\''s' \"a b\"; exit 4");
    assert_eq!(lines(&ran.text())[..2], ["it's", "a b"], "{}", ran.text());
    assert_eq!(ran.payload("exit_code"), json!(4));
}

// What follows starts no process.

/// A workspace of the test's own, and the tool for a shell that is only
/// planned for and never started.
fn planning_tool(test: &str, shell: &str) -> (Workspace, ShellTool) {
    let ws = Workspace::new(&format!("shell_plan_{test}"));
    (
        ws,
        ShellTool::new(Shell::new(shell), ShellConfig::default()),
    )
}

#[test]
fn a_call_declares_the_command_and_what_it_may_change() {
    let (ws, tool) = planning_tool("declares", "bash");
    ws.dir("sub");
    let plan = ws
        .plan(
            &tool,
            json!({ "command": "cargo test --workspace", "cwd": "sub" }),
        )
        .expect("a plan");
    assert_eq!(plan.summary, "Run `cargo test --workspace` in sub");
    assert_eq!(plan.concurrency, Concurrency::Exclusive);
    assert!(plan.has_side_effects());
    let root = ws.paths.resolve(".").expect("the root");
    let sub = ws.paths.resolve("sub").expect("the directory");
    assert_eq!(
        plan.effects,
        [
            Effect::Exec {
                command: "cargo test --workspace".to_owned(),
                cwd: sub,
            },
            Effect::Write(root.clone()),
        ]
    );

    // In the root, the summary is the command alone, shortened.
    let long = format!("echo {}\necho more", "x".repeat(200));
    let plan = ws.plan(&tool, json!({ "command": long })).expect("a plan");
    assert_eq!(plan.summary, format!("Run `echo {}…`", "x".repeat(75)));
    // The effect carries the whole command, as written.
    assert!(
        matches!(&plan.effects[0], Effect::Exec { command, cwd } if *command == long && *cwd == root)
    );
}

#[test]
fn a_directory_outside_the_workspace_is_declared_too() {
    let (ws, tool) = planning_tool("outside", "bash");
    let outside = ws.dir.join("beside");
    std::fs::create_dir_all(&outside).expect("create a directory beside the workspace");
    let plan = ws
        .plan(
            &tool,
            json!({ "command": "ls", "cwd": outside.to_str().expect("text") }),
        )
        .expect("a plan");
    assert_eq!(plan.effects.len(), 3);
    let Effect::Write(written) = &plan.effects[2] else {
        panic!("the third effect is a write: {:?}", plan.effects);
    };
    assert!(!written.is_inside_workspace());
    assert!(matches!(&plan.effects[0], Effect::Exec { cwd, .. } if cwd == written));
}

#[test]
fn a_call_that_cannot_run_as_written_is_refused_when_planned() {
    let (ws, tool) = planning_tool("refused", "bash");
    ws.file("a-file.txt", "");
    for (arguments, expected) in [
        (json!({ "command": "echo a\u{0}b" }), "NUL character"),
        (json!({ "command": "   " }), "The command is empty."),
        (
            json!({ "command": "ls", "cwd": "a-file.txt" }),
            "`cwd` is not a directory: a-file.txt",
        ),
        (
            json!({ "command": "ls", "cwd": "missing" }),
            "`cwd` is not a directory: missing",
        ),
        (json!({ "command": "ls", "timeout_ms": 0 }), "timeout_ms"),
        (
            json!({ "command": "ls", "timeout_ms": 1_800_001 }),
            "timeout_ms",
        ),
        (json!({ "command": "ls", "background": true }), "background"),
        (json!({}), "command"),
    ] {
        let error = ws
            .plan(&tool, arguments.clone())
            .expect_err("the call is refused");
        assert!(error.to_string().contains(expected), "{arguments}: {error}");
    }
    // cmd.exe reads one line, so a second one would be dropped unseen.
    let (ws, tool) = planning_tool("refused_cmd", "cmd");
    let error = ws
        .plan(&tool, json!({ "command": "echo one\necho two" }))
        .expect_err("refused");
    assert!(
        error.to_string().contains("cmd.exe reads one line"),
        "{error}"
    );
    assert!(
        ws.plan(&tool, json!({ "command": "echo one & echo two" }))
            .is_ok()
    );
}

#[test]
fn the_tool_asks_by_default_runs_alone_and_names_its_limits() {
    let config = ShellConfig {
        timeout: std::time::Duration::from_secs(5),
        max_timeout: std::time::Duration::from_secs(60),
        ..ShellConfig::default()
    };
    let tool = ShellTool::new(Shell::new("cmd"), config);
    let spec = tool.spec();
    assert_eq!(spec.name, "shell");
    assert_eq!(spec.permission, cortex_protocol::lease::RuleAction::Ask);
    assert_eq!(spec.concurrency, ConcurrencyClass::Exclusive);
    assert!(spec.description.contains("Run a command in cmd.exe on "));
    assert!(spec.description.contains("one line of cmd.exe syntax"));
    assert!(
        spec.description
            .contains("background processes are not supported")
    );
    assert_eq!(
        spec.parameters["properties"]["timeout_ms"]["maximum"],
        json!(60_000)
    );
    assert!(
        spec.parameters["properties"]["timeout_ms"]["description"]
            .as_str()
            .expect("text")
            .contains("5000 unless given")
    );

    // It is one of the declared tools of a session.
    let toolbox = Toolbox::read_only(Arc::new(ToolRegistry::new())).with(tool);
    assert!(toolbox.names().any(|name| name == "shell"));
    assert_eq!(toolbox.len(), 6);
}

#[test]
fn sys_a_shell_that_is_not_there_is_an_error_for_the_model() {
    let bench = Bench::new("shell_missing", Shell::new("no-such-shell-program-here"));
    let ran = bench.command("echo hi");
    assert_eq!(ran.result.outcome(), &ToolOutcome::Error);
    let text = ran.text();
    assert!(
        text.starts_with("The command was not started: `no-such-shell-program-here`"),
        "{text}"
    );
    assert!(
        text.contains("The shell is no-such-shell-program-here (a POSIX shell)."),
        "{text}"
    );
}
