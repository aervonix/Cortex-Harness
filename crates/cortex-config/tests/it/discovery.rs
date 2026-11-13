//! Finding other tools' files, against made-up fixtures.
//!
//! The "home directory" here is a folder of the test's own, and the files
//! in it are empty or hold a marker that must never be read. The real home
//! directory is not looked at: discovery takes its home from the [`Env`] it
//! is given.

use std::fs;
use std::path::Path;

use cortex_config::discover::{Found, FoundKind, Scope, discover};
use cortex_config::schema::CompatSettings;
use cortex_config::{Env, Platform};

use crate::{Scratch, write};

/// What a fixture file holds. Nothing may ever read it.
const MARKER: &str =
    "{ \"mcpServers\": { \"secret\": { \"env\": { \"TOKEN\": \"sk-test-never-read\" } } } }";

fn env(home: &Path) -> Env {
    let variable = match Platform::CURRENT {
        Platform::Unix => "HOME",
        Platform::Windows => "USERPROFILE",
    };
    Env::empty(Platform::CURRENT).with(variable, home.to_str().unwrap())
}

fn summary(found: &[Found], base: &Path) -> Vec<String> {
    found
        .iter()
        .map(|found| {
            let path = found
                .path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            format!("{:?} {:?} {}: {path}", found.scope, found.kind, found.from)
        })
        .collect()
}

fn all() -> CompatSettings {
    CompatSettings::default()
}

#[test]
fn nothing_is_found_where_nothing_is() {
    let scratch = Scratch::new("discover_nothing");
    let home = scratch.join("home");
    let workspace = scratch.join("home/work/app");
    fs::create_dir_all(&workspace).unwrap();
    assert!(discover(&env(&home), Some(&workspace), &all()).is_empty());
    // With no home directory and no workspace there is nowhere to look.
    assert!(discover(&Env::empty(Platform::CURRENT), None, &all()).is_empty());
}

#[test]
fn what_other_tools_left_is_reported_with_whose_it_is() {
    let scratch = Scratch::new("discover_found");
    let home = scratch.join("home");
    let workspace = home.join("work").join("app");
    for file in [
        "work/app/AGENTS.md",
        "work/app/CLAUDE.md",
        "work/app/.mcp.json",
        "work/app/.cursor/mcp.json",
        "work/app/.vscode/mcp.json",
        ".claude.json",
        ".cursor/mcp.json",
        ".codex/config.toml",
    ] {
        write(&home.join(file), MARKER);
    }
    for folder in [
        "work/app/.agents/skills/review",
        "work/app/.claude/skills",
        ".agents/skills",
        ".claude/skills/commit",
    ] {
        fs::create_dir_all(home.join(folder)).unwrap();
    }

    let found = discover(&env(&home), Some(&workspace), &all());
    assert_eq!(
        summary(&found, &home),
        [
            "Project Instructions AGENTS.md: work/app/AGENTS.md",
            "Project Instructions Claude Code: work/app/CLAUDE.md",
            "Project McpServers the project's .mcp.json: work/app/.mcp.json",
            "Project McpServers Cursor: work/app/.cursor/mcp.json",
            "Project McpServers VS Code: work/app/.vscode/mcp.json",
            "Project Skills Agent Skills: work/app/.agents/skills",
            "Project Skills Claude Code: work/app/.claude/skills",
            "User McpServers Claude Code: .claude.json",
            "User McpServers Cursor: .cursor/mcp.json",
            "User McpServers Codex: .codex/config.toml",
            "User Skills Agent Skills: .agents/skills",
            "User Skills Claude Code: .claude/skills",
        ]
    );
    assert!(found.iter().any(|found| found.scope == Scope::Project));

    // Without a workspace, only the user's.
    let found = discover(&env(&home), None, &all());
    assert_eq!(found.len(), 5);
    assert!(found.iter().all(|found| found.scope == Scope::User));
}

#[test]
fn each_kind_can_be_turned_off() {
    let scratch = Scratch::new("discover_compat");
    let home = scratch.join("home");
    let workspace = home.join("work");
    write(&workspace.join("AGENTS.md"), "");
    write(&workspace.join(".mcp.json"), "");
    write(&home.join(".claude.json"), "");
    fs::create_dir_all(workspace.join(".claude/skills")).unwrap();

    let kinds = |compat: &CompatSettings| -> Vec<FoundKind> {
        discover(&env(&home), Some(&workspace), compat)
            .into_iter()
            .map(|found| found.kind)
            .collect()
    };
    assert_eq!(
        kinds(&all()),
        [
            FoundKind::Instructions,
            FoundKind::McpServers,
            FoundKind::Skills,
            FoundKind::McpServers
        ]
    );
    assert_eq!(
        kinds(&CompatSettings {
            mcp: false,
            ..all()
        }),
        [FoundKind::Instructions, FoundKind::Skills]
    );
    assert_eq!(
        kinds(&CompatSettings {
            instructions: false,
            skills: false,
            ..all()
        }),
        [FoundKind::McpServers, FoundKind::McpServers]
    );
    assert!(
        kinds(&CompatSettings {
            instructions: false,
            mcp: false,
            skills: false,
        })
        .is_empty()
    );
}

#[test]
fn a_file_must_be_a_file_and_a_folder_a_folder() {
    let scratch = Scratch::new("discover_shapes");
    let home = scratch.join("home");
    let workspace = home.join("work");
    // A folder named like the file, and a file named like the folder.
    fs::create_dir_all(workspace.join("AGENTS.md")).unwrap();
    fs::create_dir_all(workspace.join(".claude")).unwrap();
    write(&workspace.join(".claude/skills"), "not a folder");
    assert!(discover(&env(&home), Some(&workspace), &all()).is_empty());
}

#[test]
fn the_desktop_clients_file_is_looked_for_in_the_configuration_folder() {
    let scratch = Scratch::new("discover_desktop");
    let home = scratch.join("home");
    let (variable, base) = match Platform::CURRENT {
        Platform::Unix => ("XDG_CONFIG_HOME", scratch.join("xdg")),
        Platform::Windows => ("APPDATA", scratch.join("roaming")),
    };
    write(&base.join("Claude/claude_desktop_config.json"), MARKER);
    let env = env(&home).with(variable, base.to_str().unwrap());
    let found = discover(&env, None, &all());
    assert_eq!(
        summary(&found, &base),
        ["User McpServers Claude Desktop: Claude/claude_desktop_config.json"]
    );
    assert!(
        discover(
            &env,
            None,
            &CompatSettings {
                mcp: false,
                ..all()
            }
        )
        .is_empty()
    );
}

/// Discovery asks whether a file is there and nothing more. On Unix that is
/// shown by taking away the permission to read the files: they are found
/// all the same, which they could not be if anything opened them.
#[cfg(unix)]
#[test]
fn nothing_that_is_found_is_opened() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("discover_unreadable");
    let home = scratch.join("home");
    let workspace = home.join("work");
    let files = [
        workspace.join(".mcp.json"),
        home.join(".claude.json"),
        home.join(".codex/config.toml"),
    ];
    for file in &files {
        write(file, MARKER);
        fs::set_permissions(file, fs::Permissions::from_mode(0o000)).unwrap();
    }
    // Root can read anything, so the proof would prove nothing there.
    if fs::read(&files[0]).is_ok() {
        println!("skipped: this user can read a file with no permissions");
        return;
    }

    let before: Vec<_> = files
        .iter()
        .map(|file| fs::metadata(file).unwrap().accessed().ok())
        .collect();
    let found = discover(&env(&home), Some(&workspace), &all());
    assert_eq!(found.len(), 3, "{found:?}");
    let after: Vec<_> = files
        .iter()
        .map(|file| fs::metadata(file).unwrap().accessed().ok())
        .collect();
    assert_eq!(before, after, "no file's access time moved");
    assert!(!format!("{found:?}").contains("sk-test-never-read"));
}
