//! Integration tests (tier 2): sessions on a real file system.
//!
//! Every test works in a directory of its own under Cargo's temporary
//! directory. Time comes from the fake clock; nothing waits on a real one.
//! Tests that start git are system tests (`sys_…`).

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::missing_panics_doc,
    reason = "an integration test prepares real files, fails by panicking, and prints measurements"
)]

mod base;
mod durability;
mod index;
mod perf;
mod private;
mod snapshot;
mod store;
mod tree;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use cortex_base::{AbsPath, FakeClock};
use cortex_protocol::ids::{CallId, ItemId, SessionId};
use cortex_protocol::item::{
    AssistantMessageItem, Item, ToolOutcome, ToolResultItem, UserMessageItem,
};
use cortex_protocol::message::{Part, ToolCallPart};
use cortex_session::{Body, Durability, ProjectSessions, Session, SessionOptions};
pub(crate) use cortex_testkit::Scratch;
use serde_json::json;

/// A workspace and a sessions directory for one test.
pub(crate) struct World {
    pub(crate) scratch: Scratch,
    pub(crate) project: ProjectSessions,
    pub(crate) workspace: PathBuf,
    pub(crate) root: AbsPath,
    pub(crate) clock: FakeClock,
}

impl World {
    pub(crate) fn new(test: &str) -> Self {
        let scratch = cortex_testkit::scratch!(test);
        let workspace = scratch.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let root = AbsPath::from_os(&fs::canonicalize(&workspace).unwrap()).unwrap();
        let project =
            ProjectSessions::at(scratch.join("sessions").join(root.project_key().as_str()));
        Self {
            scratch,
            project,
            workspace,
            root,
            clock: FakeClock::starting_at("2026-11-12T10:00:00Z").unwrap(),
        }
    }

    /// Options for a test: nothing is flushed to the disk, because a flush
    /// takes milliseconds and what a test checks is what the file holds.
    /// The tests of flushing set their own durability.
    pub(crate) fn options(&self) -> SessionOptions {
        SessionOptions {
            durability: Durability::Never,
            ..SessionOptions::new(Arc::new(self.clock.clone()), "0.0.0-test")
        }
    }

    pub(crate) fn options_with(&self, durability: Durability) -> SessionOptions {
        SessionOptions {
            durability,
            ..self.options()
        }
    }

    pub(crate) fn create(&self, id: &str) -> Session {
        Session::create(
            &self.project,
            SessionId::new(id),
            &self.root,
            self.options(),
        )
        .unwrap()
    }

    pub(crate) fn resume(&self, id: &str) -> (Session, cortex_session::OpenReport) {
        Session::resume(&self.project, &SessionId::new(id), self.options()).unwrap()
    }

    pub(crate) fn file(&self, relative: &str) -> PathBuf {
        self.workspace.join(relative)
    }

    pub(crate) fn abs(&self, relative: &str) -> AbsPath {
        self.root.join(relative).unwrap()
    }

    /// Every file of the workspace and its content.
    pub(crate) fn contents(&self) -> BTreeMap<String, Vec<u8>> {
        let mut found = BTreeMap::new();
        let mut stack = vec![(self.workspace.clone(), String::new())];
        while let Some((directory, prefix)) = stack.pop() {
            for entry in fs::read_dir(&directory).unwrap().flatten() {
                let name = entry.file_name().into_string().unwrap();
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.file_type().unwrap().is_dir() {
                    stack.push((entry.path(), path));
                } else {
                    found.insert(path, fs::read(entry.path()).unwrap());
                }
            }
        }
        found
    }

    pub(crate) fn write(&self, relative: &str, content: &[u8]) {
        let path = self.file(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }
}

pub(crate) fn user(text: &str) -> Body {
    Body::User(UserMessageItem {
        id: ItemId::new(format!("i_u{}", text.len())),
        parts: vec![Part::text(text)],
        delivery: None,
        created_at: None,
    })
}

pub(crate) fn user_item(id: &str, text: &str) -> Item {
    Item::UserMessage(UserMessageItem {
        id: ItemId::new(id),
        parts: vec![Part::text(text)],
        delivery: None,
        created_at: None,
    })
}

pub(crate) fn assistant(id: &str, text: &str, calls: &[&str]) -> Body {
    let mut parts = vec![Part::text(text)];
    parts.extend(calls.iter().map(|call| {
        Part::ToolCall(ToolCallPart {
            id: CallId::new(*call),
            name: "read".into(),
            arguments: json!({"path": "src/lib.rs"}),
            raw: None,
        })
    }));
    Body::Assistant {
        item: AssistantMessageItem {
            id: ItemId::new(id),
            parts,
            stop_reason: None,
            raw_stop: None,
            stop_detail: None,
            truncated: false,
            origin: None,
            usage: None,
            error: None,
            created_at: None,
        },
        request_id: Some("req_1".into()),
    }
}

pub(crate) fn tool_result(id: &str, call: &str) -> Body {
    Body::ToolResult(ToolResultItem {
        id: ItemId::new(id),
        call_id: CallId::new(call),
        tool: "read".into(),
        outcome: ToolOutcome::Success,
        content: vec![Part::text("fn main() {}")],
        payload: None,
        artifacts: Vec::new(),
        duration_ms: Some(3),
        created_at: None,
    })
}

/// Whether the tests run in CI, where a missing tool is a failure and not
/// a reason to skip.
pub(crate) fn in_ci() -> bool {
    std::env::var_os("CI").is_some()
}
