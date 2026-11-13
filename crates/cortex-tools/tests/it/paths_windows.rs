//! The Windows path cases of `docs/architecture/05-tools.md` ("Windows
//! differences in one place"), through the tools: what a model may write as
//! a path, what it is shown, and what is refused.
//!
//! The resolver's own cases are in `cortex-base`. These check that every
//! tool here hands its paths to it, and that a walk steps past a name the
//! rules refuse.

use std::fs;

use cortex_tools::{Effect, ListTool, ReadTool, SearchTool, Tool};
use serde_json::json;

use crate::{Workspace, failed, ok, skipped};

fn project(test: &str) -> Workspace {
    let ws = Workspace::new(test);
    ws.file("src/Main.rs", "fn main() {}\n// needle\n");
    ws.file("src/util/Text.rs", "// needle\n");
    ws.file("README.md", "needle\n");
    ws
}

#[test]
fn both_separators_are_accepted_and_the_forward_one_is_shown() {
    let ws = project("win_separators");
    let read = ReadTool::new();
    for path in [
        r"src\Main.rs",
        "src/Main.rs",
        r".\src\util\..\Main.rs",
        r"src\util/../Main.rs",
    ] {
        let plan = ws.plan(&read, json!({"path": path})).unwrap();
        assert_eq!(plan.summary, "Read src/Main.rs", "{path}");
        assert!(
            matches!(plan.effects.as_slice(), [Effect::Read(found)] if found.display() == "src/Main.rs")
        );
    }
    assert_eq!(
        ok(&ws.call(&read, json!({"path": r"src\Main.rs"}))),
        "src/Main.rs (2 lines)\nfn main() {}\n// needle"
    );
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({"path": r"src\util"}))),
        "1 entry in `src/util`:\nsrc/util/Text.rs"
    );
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "path": r"src\util", "mode": "files"})
        )),
        "1 file with a match for `needle` in `src/util`:\nsrc/util/Text.rs"
    );
}

#[test]
fn a_glob_may_be_written_with_either_separator_and_ignores_case() {
    let ws = project("win_glob");
    for glob in [r"src\**\*.rs", "src/**/*.rs", "SRC/**/*.RS"] {
        assert_eq!(
            ok(&ws.call(&ListTool::new(), json!({"glob": glob}))),
            format!("2 entries matching `{glob}` under `.`:\nsrc/Main.rs\nsrc/util/Text.rs"),
            "{glob}"
        );
    }
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "glob": "*.MD", "mode": "files"})
        )),
        "1 file with a match for `needle` in `.` (files matching `*.MD`):\nREADME.md"
    );
}

#[test]
fn names_are_compared_without_regard_to_case_and_shown_as_they_are_on_disk() {
    let ws = project("win_case");
    let read = ReadTool::new();
    // Asked for in another case, the file is found, and shown as spelled
    // on disk.
    assert_eq!(
        ok(&ws.call(&read, json!({"path": "SRC/MAIN.RS"}))),
        "src/Main.rs (2 lines)\nfn main() {}\n// needle"
    );
    // Both spellings are one seen file: the second read is a re-read.
    let again = ok(&ws.call(&read, json!({"path": "src/main.rs"})));
    assert!(
        again.starts_with("src/Main.rs: unchanged since you read lines 1-2"),
        "{again}"
    );
    assert_eq!(ws.files.seen().len(), 1);
    // So a rule on the file's path is matched whatever case was typed.
    let first = ws.plan(&read, json!({"path": "SRC/MAIN.RS"})).unwrap();
    let second = ws.plan(&read, json!({"path": "src/Main.rs"})).unwrap();
    assert_eq!(first.effects, second.effects);
}

#[test]
fn an_absolute_path_with_a_drive_letter_is_read_and_shown_inside_the_workspace() {
    let ws = project("win_absolute");
    let root = ws.root();
    let absolute = root.join("src").join("Main.rs");
    let written = absolute.to_str().expect("the test directory is Unicode");
    assert_eq!(
        ok(&ws.call(&ReadTool::new(), json!({"path": written}))),
        "src/Main.rs (2 lines)\nfn main() {}\n// needle"
    );
    // The same with forward slashes.
    let forward = written.replace('\\', "/");
    let plan = ws.plan(&ReadTool::new(), json!({"path": forward})).unwrap();
    assert_eq!(plan.summary, "Read src/Main.rs");
}

/// Every tool refuses `path` with a message that holds `reason`, when it
/// plans: before anything is opened, and before anything is asked of the
/// user.
#[track_caller]
fn refused_by_every_tool(ws: &Workspace, path: &str, reason: &str) {
    let tools: [(&dyn Tool, serde_json::Value); 3] = [
        (&ReadTool::new(), json!({"path": path})),
        (&ListTool::new(), json!({"path": path})),
        (
            &SearchTool::new(),
            json!({"pattern": "needle", "path": path}),
        ),
    ];
    for (tool, arguments) in tools {
        let name = tool.spec().name.clone();
        let error = match ws.plan(tool, arguments.clone()) {
            Ok(plan) => panic!("`{name}` planned {path:?}: {plan:?}"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(reason), "`{name}` on {path:?}: {error}");
        assert_eq!(failed(&ws.call(tool, arguments)), error);
    }
}

#[test]
fn device_names_are_refused() {
    let ws = project("win_devices");
    for path in [
        "NUL",
        "nul",
        "CON",
        "con.txt",
        "src/COM1",
        r"src\lpt9.log",
        "AUX",
        "PRN.rs",
        "CONIN$",
    ] {
        refused_by_every_tool(&ws, path, "is a device name on Windows");
    }
}

#[test]
fn device_and_extended_length_paths_are_refused() {
    let ws = project("win_device_paths");
    for path in [
        r"\\.\C:\Windows\win.ini",
        r"\\?\C:\Windows\win.ini",
        r"\\.\NUL",
        r"\\?\UNC\server\share\x",
    ] {
        refused_by_every_tool(&ws, path, "device or extended-length path");
    }
}

#[test]
fn trailing_dots_and_spaces_and_alternate_data_streams_are_refused() {
    let ws = project("win_trailing");
    for path in ["README.md.", "README.md ", "src./Main.rs", "src /Main.rs"] {
        refused_by_every_tool(&ws, path, "ends with a dot or a space");
    }
    for path in [
        "README.md::$DATA",
        "README.md:stream",
        "src/Main.rs:Zone.Identifier",
    ] {
        refused_by_every_tool(&ws, path, "which Windows does not allow in a file name");
    }
    for path in ["what?.txt", "a*b", "x<y", "pipe|name", "quote\"d"] {
        refused_by_every_tool(&ws, path, "which Windows does not allow in a file name");
    }
}

#[test]
fn drive_relative_and_root_relative_paths_are_refused() {
    let ws = project("win_relative");
    refused_by_every_tool(
        &ws,
        "C:README.md",
        "relative to the current directory of a drive",
    );
    refused_by_every_tool(
        &ws,
        r"C:src\Main.rs",
        "relative to the current directory of a drive",
    );
    refused_by_every_tool(
        &ws,
        r"\Windows\win.ini",
        "starts at the root of whichever drive is current",
    );
    refused_by_every_tool(
        &ws,
        "/Windows/win.ini",
        "starts at the root of whichever drive is current",
    );
}

#[test]
fn network_paths_are_refused_on_their_text() {
    let ws = project("win_unc");
    // No share is contacted: the refusal is made on the text, in `plan`.
    // The server's name is one that cannot exist.
    for path in [
        r"\\cortex-test.invalid\share\notes.txt",
        "//cortex-test.invalid/share/notes.txt",
    ] {
        refused_by_every_tool(&ws, path, "which the configuration does not allow");
    }
    refused_by_every_tool(&ws, r"\\cortex-test.invalid", "needs a server and a share");
}

#[test]
fn a_walk_steps_past_a_name_the_rules_refuse_and_says_so() {
    let ws = project("win_walk_refused");
    ws.file("docs/guide.md", "needle\n");
    // Through an extended-length path, Windows creates names that an
    // ordinary path cannot spell: one that ends with a dot, and one that is
    // a device's.
    let verbatim = fs::canonicalize(ws.root())
        .expect("the workspace exists")
        .join("docs");
    let made = fs::write(verbatim.join("trailing."), "needle\n").is_ok()
        && fs::write(verbatim.join("aux"), "needle\n").is_ok();
    if !made {
        skipped("this volume refuses the names the test needs");
        return;
    }

    let text = ok(&ws.call(&ListTool::new(), json!({"path": "docs"})));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        &lines[..2],
        ["1 entry in `docs`:", "docs/guide.md"],
        "{text}"
    );
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[2].starts_with("[Skipped 2 entries whose path cannot be used. For example: "),
        "{text}"
    );
    assert!(
        lines[2].contains("ends with a dot or a space")
            || lines[2].contains("is a device name on Windows"),
        "{text}"
    );

    // The search goes on past them, and finds what it may use.
    let text = ok(&ws.call(
        &SearchTool::new(),
        json!({"pattern": "needle", "path": "docs", "mode": "files"}),
    ));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        &lines[..2],
        [
            "1 file with a match for `needle` in `docs`:",
            "docs/guide.md"
        ],
        "{text}"
    );
    assert!(
        lines[2].starts_with("[Skipped 2 entries whose path cannot be used."),
        "{text}"
    );
}

#[test]
fn a_path_longer_than_260_characters_is_read_listed_and_searched() {
    let ws = Workspace::new("win_long_path");
    let deep: String = (0..12)
        .map(|n| format!("directory-number-{n:02}-of-a-long-path"))
        .collect::<Vec<_>>()
        .join("/");
    assert!(deep.len() > 260);
    // The test itself needs the extended-length form to make the tree.
    let verbatim = fs::canonicalize(ws.root()).expect("the workspace exists");
    let directory = deep.split('/').fold(verbatim, |path, name| path.join(name));
    fs::create_dir_all(&directory).expect("create the deep directory");
    fs::write(directory.join("notes.txt"), "needle\n").expect("write the deep file");

    let file = format!("{deep}/notes.txt");
    assert_eq!(
        ok(&ws.call(&ReadTool::new(), json!({"path": file}))),
        format!("{file} (1 line)\nneedle")
    );
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({"path": deep}))),
        format!("1 entry in `{deep}`:\n{file}")
    );
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "mode": "files"})
        )),
        format!("1 file with a match for `needle` in `.`:\n{file}")
    );
}

#[test]
fn ignore_files_are_honoured_on_windows_too() {
    let ws = project("win_ignore");
    ws.file(".gitignore", "target/\r\n*.log\r\n");
    ws.file("target/debug/app.rs", "needle\n");
    ws.file("build.log", "needle\n");
    assert_eq!(
        ok(&ws.call(
            &SearchTool::new(),
            json!({"pattern": "needle", "mode": "files"})
        )),
        "3 files with a match for `needle` in `.`:\nREADME.md\nsrc/Main.rs\nsrc/util/Text.rs"
    );
    assert_eq!(
        ok(&ws.call(&ListTool::new(), json!({}))),
        "2 entries in `.`:\nREADME.md\nsrc/"
    );
}
