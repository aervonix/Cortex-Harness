//! Finding what other tools left on the machine
//! (`docs/architecture/08-configuration.md`, "Reading other tools'
//! set-up").
//!
//! People switching from another agent should not have to rewrite anything.
//! This module answers one question: **what is there?** It reports the
//! instruction files, MCP server definitions and Agent Skills folders it
//! finds, for the onboarding flow and the import commands to offer.
//!
//! **Nothing is opened.** Discovery asks the file system whether a path
//! exists and what kind of thing it is, and nothing more. Another tool's
//! file can hold its tokens and its user's history, and reading one is an
//! import, which the user asks for and a later step builds. And nothing is
//! ever written.
//!
//! The lists of other tools' locations were written down on 2026-11-12 and
//! go stale. The steps that read the files (MCP import, skills) confirm
//! them against each tool's current documentation.

#![allow(
    clippy::disallowed_methods,
    reason = "a stat call per candidate path; runs at onboarding and on request, on the blocking pool"
)]

use std::path::{Path, PathBuf};

use crate::dirs::ProjectLocator;
use crate::env::{Env, Platform};
use crate::schema::CompatSettings;

/// What kind of thing was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FoundKind {
    /// A file of instructions for a coding agent: `AGENTS.md`, `CLAUDE.md`.
    Instructions,
    /// A file that defines MCP servers for another client.
    McpServers,
    /// A folder of Agent Skills.
    Skills,
}

/// Whose it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// In the workspace: it came with the repository, and needs the same
    /// trust as anything else a project brings.
    Project,
    /// In the user's home directory.
    User,
}

/// One thing that was found.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Found {
    /// What kind of thing it is.
    pub kind: FoundKind,
    /// Whose it is.
    pub scope: Scope,
    /// The tool or convention it belongs to, as a name to show.
    pub from: &'static str,
    /// Where it is.
    pub path: PathBuf,
}

/// Whether a candidate is a file or a folder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    File,
    Folder,
}

/// A place something may be, relative to the workspace or the home
/// directory.
struct Candidate {
    kind: FoundKind,
    from: &'static str,
    path: &'static [&'static str],
    shape: Shape,
}

const fn file(kind: FoundKind, from: &'static str, path: &'static [&'static str]) -> Candidate {
    Candidate {
        kind,
        from,
        path,
        shape: Shape::File,
    }
}

const fn folder(kind: FoundKind, from: &'static str, path: &'static [&'static str]) -> Candidate {
    Candidate {
        kind,
        from,
        path,
        shape: Shape::Folder,
    }
}

/// What is looked for in a workspace.
const IN_PROJECT: &[Candidate] = &[
    file(FoundKind::Instructions, "AGENTS.md", &["AGENTS.md"]),
    file(FoundKind::Instructions, "Claude Code", &["CLAUDE.md"]),
    file(
        FoundKind::McpServers,
        "the project's .mcp.json",
        &[".mcp.json"],
    ),
    file(FoundKind::McpServers, "Cursor", &[".cursor", "mcp.json"]),
    file(FoundKind::McpServers, "VS Code", &[".vscode", "mcp.json"]),
    folder(FoundKind::Skills, "Agent Skills", &[".agents", "skills"]),
    folder(FoundKind::Skills, "Claude Code", &[".claude", "skills"]),
];

/// What is looked for in the home directory.
const IN_HOME: &[Candidate] = &[
    file(FoundKind::McpServers, "Claude Code", &[".claude.json"]),
    file(FoundKind::McpServers, "Cursor", &[".cursor", "mcp.json"]),
    file(FoundKind::McpServers, "Codex", &[".codex", "config.toml"]),
    folder(FoundKind::Skills, "Agent Skills", &[".agents", "skills"]),
    folder(FoundKind::Skills, "Claude Code", &[".claude", "skills"]),
];

/// Reports what other tools left in the workspace and in the user's home
/// directory, in a fixed order. A kind that `compat` turns off is not
/// looked for.
///
/// The home directory comes from `env`, never from the process, so a test
/// looks only where it says.
#[must_use]
pub fn discover(env: &Env, workspace: Option<&Path>, compat: &CompatSettings) -> Vec<Found> {
    let wanted = |kind: FoundKind| match kind {
        FoundKind::Instructions => compat.instructions,
        FoundKind::McpServers => compat.mcp,
        FoundKind::Skills => compat.skills,
    };
    let mut found = Vec::new();
    // In the workspace, a candidate is looked at where it really is, found
    // through the path resolver: a repository can put a link where another
    // tool's file goes, and on Windows merely asking about a link to a
    // network share sends the account's credentials to that server. A
    // candidate whose way is refused is not reported. The home directory is
    // the user's own, and is asked as it is.
    let locator = workspace.map(ProjectLocator::new);
    let mut look = |base: &Path, scope: Scope, candidates: &[Candidate]| {
        for candidate in candidates.iter().filter(|candidate| wanted(candidate.kind)) {
            let mut path = base.to_path_buf();
            path.extend(candidate.path);
            let real = match (&locator, scope) {
                (Some(locator), Scope::Project) => match locator.locate(&path) {
                    Ok(Some(real)) => real,
                    Ok(None) | Err(_) => continue,
                },
                _ => path.clone(),
            };
            if exists_as(&real, candidate.shape) {
                found.push(Found {
                    kind: candidate.kind,
                    scope,
                    from: candidate.from,
                    path,
                });
            }
        }
    };
    if let Some(workspace) = workspace {
        look(workspace, Scope::Project, IN_PROJECT);
    }
    if let Some(home) = env.home() {
        look(&home, Scope::User, IN_HOME);
    }
    // One more per-user file, whose folder differs by OS.
    if compat.mcp {
        let desktop = match env.platform() {
            Platform::Unix => env
                .absolute_path("XDG_CONFIG_HOME")
                .or_else(|| env.home().map(|home| home.join(".config"))),
            Platform::Windows => env.absolute_path("APPDATA"),
        }
        .map(|base| base.join("Claude").join("claude_desktop_config.json"));
        if let Some(path) = desktop.filter(|path| exists_as(path, Shape::File)) {
            found.push(Found {
                kind: FoundKind::McpServers,
                scope: Scope::User,
                from: "Claude Desktop",
                path,
            });
        }
    }
    found
}

/// Whether `path` is there, as a file or as a folder. A link is followed:
/// in the workspace, `path` is where the resolver found the candidate, so
/// there is none left to follow. The file is not opened.
fn exists_as(path: &Path, shape: Shape) -> bool {
    std::fs::metadata(path).is_ok_and(|found| match shape {
        Shape::File => found.is_file(),
        Shape::Folder => found.is_dir(),
    })
}
