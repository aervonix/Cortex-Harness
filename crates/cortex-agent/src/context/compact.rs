//! Compaction: the summary request, and the state carried across it
//! mechanically (`docs/architecture/07-sessions-and-context.md`,
//! "Compaction").
//!
//! What must not depend on a summary's accuracy is copied, not summarised:
//! the plan, the files read and changed with their hashes, the user's last
//! message word for word, and the artifacts of the last few rounds. They go
//! in the `summary` entry twice: as data (`carried`, which the core reads
//! the plan back from) and as text in the summary the model is sent.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use cortex_base::AbsPath;
use cortex_protocol::item::{ArtifactRef, PlanStatus, ToolOutcome};
use cortex_protocol::{Item, Message, Part, Plan, Role};
use cortex_tools::SeenFile;
use serde_json::{Value, json};

use super::history::NOTICE_LEAD;
use super::instructions::{date_text, shown};

/// What the model is asked, at the end of the epoch, for its summary.
/// Written for this project.
pub const SUMMARY_INSTRUCTIONS: &str = "\
The conversation so far is about to be replaced by a summary, so that the \
work can go on in a fresh context. Write that summary now. Besides it, the \
next context will have only the plan, the list of files read and changed, \
the user's last message and the artifacts of the last few rounds, which \
the harness carries across as they are.

Do not call any tool. Answer with the summary alone, under these headings:

1. Goal: what the user asked for, with every constraint and preference they \
stated.
2. Done: what has been done so far, and how it was checked.
3. Decisions: what was decided and why, with the approaches that were tried \
and dropped.
4. State: where the work stands: what is finished, what is half done, what \
is broken.
5. Next: what remains to do, in order.
6. Exact text: names, paths, commands, error messages and values that must \
be kept word for word.

Be specific and brief. Leave out what no longer matters.";

/// How many of the last rounds' artifacts are carried across.
pub(crate) const ARTIFACT_ROUNDS: usize = 3;

/// The request's last message: the instructions, and the user's focus.
pub(crate) fn instructions_message(focus: Option<&str>) -> Message {
    let mut text = format!("{NOTICE_LEAD}{SUMMARY_INSTRUCTIONS}");
    if let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        let _ = write!(
            text,
            "\n\nThe user asks the summary to keep in particular: {focus}"
        );
    }
    Message {
        role: Role::User,
        parts: vec![Part::text(text)],
        origin: None,
    }
}

/// A file as the carried state lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CarriedFile {
    /// Relative to the workspace with `/`, or absolute outside it.
    pub(crate) path: String,
    pub(crate) read: bool,
    pub(crate) changed: bool,
    /// The hash of what a tool last found or left, in hexadecimal.
    pub(crate) hash: Option<String>,
}

/// The kinds of a file tool's payload that say its file was written.
const WRITTEN: &[&str] = &["create", "write", "edit"];

/// The files the tools' results on the branch say were read or changed,
/// by the path each payload names: `path` with the kinds the file tools
/// give it, and the shell's `changes`, read as the headless report reads
/// them (`cortex-cli`'s `report`). What the table of seen files knows is
/// this process's, and is forgotten at a rewind; this is the branch's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Touched {
    /// Each path as its payload names it: whether it was read, changed.
    files: BTreeMap<String, (bool, bool)>,
}

impl Touched {
    /// Notes what a recorded item says of files.
    pub(crate) fn note(&mut self, item: &Item) {
        let Item::ToolResult(result) = item else {
            return;
        };
        let Some(payload) = &result.payload else {
            return;
        };
        if result.outcome == ToolOutcome::Success
            && let Some(path) = payload.get("path").and_then(Value::as_str)
        {
            let kind = payload.get("kind").and_then(Value::as_str);
            if kind.is_some_and(|kind| WRITTEN.contains(&kind)) {
                self.files.entry(path.to_owned()).or_default().1 = true;
            } else if result.tool == "read" {
                self.files.entry(path.to_owned()).or_default().0 = true;
            }
        }
        // A command's changes count whatever it exited with.
        if let Some(changes) = payload.get("changes") {
            for list in ["created", "modified", "deleted"] {
                for path in changes
                    .get(list)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    self.files.entry(path.to_owned()).or_default().1 = true;
                }
            }
        }
    }
}

/// The state carried across a compaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Carried {
    pub(crate) plan: Option<Plan>,
    pub(crate) files: Vec<CarriedFile>,
    pub(crate) last_user: Option<Vec<Part>>,
    pub(crate) artifacts: Vec<ArtifactRef>,
}

impl Carried {
    /// The files read and changed: the ones the last summary on the branch
    /// carried, then the ones the branch's tool results touched since
    /// ([`Touched`], which clears a hash it cannot vouch for), then what
    /// this process's table knows, whose hash wins. A resumed session's
    /// table starts empty, and a rewind empties it.
    pub(crate) fn files_from(
        root: &AbsPath,
        table: &[SeenFile],
        touched: &Touched,
        earlier: Option<&Value>,
    ) -> Vec<CarriedFile> {
        let mut files: Vec<CarriedFile> = earlier
            .and_then(|carried| carried.get("files"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|file| {
                        Some(CarriedFile {
                            path: file.get("path")?.as_str()?.to_owned(),
                            read: file.get("read").and_then(Value::as_bool).unwrap_or(false),
                            changed: file
                                .get("changed")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            hash: file.get("hash").and_then(Value::as_str).map(str::to_owned),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // What the branch says since: a hash from before is not the file's
        // any more, and this process's table gives it back if it has one.
        for (named, &(read, changed)) in &touched.files {
            let path = root
                .join(named)
                .or_else(|_| AbsPath::parse(named, root.style()))
                .map_or_else(|_| named.clone(), |abs| shown(root, &abs));
            match files.iter_mut().find(|known| known.path == path) {
                Some(known) => {
                    known.read |= read;
                    known.changed |= changed;
                    known.hash = None;
                }
                None => files.push(CarriedFile {
                    path,
                    read,
                    changed,
                    hash: None,
                }),
            }
        }
        for seen in table {
            let path = shown(root, &seen.path);
            let file = CarriedFile {
                path: path.clone(),
                read: seen.read,
                changed: seen.changed,
                hash: seen
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.hash.to_hex()),
            };
            match files.iter_mut().find(|known| known.path == path) {
                Some(known) => {
                    known.read |= file.read;
                    known.changed |= file.changed;
                    known.hash = file.hash;
                }
                None => files.push(file),
            }
        }
        files.sort_by(|one, other| one.path.cmp(&other.path));
        files
    }

    /// The carried state as the `summary` entry stores it.
    pub(crate) fn to_value(&self) -> Value {
        json!({
            "plan": self.plan,
            "files": self.files.iter().map(|file| json!({
                "path": file.path,
                "read": file.read,
                "changed": file.changed,
                "hash": file.hash,
            })).collect::<Vec<_>>(),
            "last_user": self.last_user,
            "artifacts": self.artifacts,
        })
    }

    /// The text the model is sent: the summary, then the carried state.
    pub(crate) fn render(&self, summary: &str, date: &str) -> String {
        let mut out = format!(
            "This conversation was compacted to make room. {} What came before is summarised \
             below. The plan, the files and the user's last message were carried across exactly \
             as they were.\n\n## Summary\n\n{}",
            date_text(date),
            summary.trim()
        );
        out.push_str("\n\n## Plan\n\n");
        match &self.plan {
            Some(plan) if !plan.items.is_empty() => {
                for item in &plan.items {
                    let status = match &item.status {
                        PlanStatus::Pending => "pending",
                        PlanStatus::Active => "active",
                        PlanStatus::Done => "done",
                        PlanStatus::Skipped => "skipped",
                        other => other.as_str(),
                    };
                    let _ = writeln!(out, "- [{status}] {} ({})", item.text, item.id);
                }
                out.pop();
            }
            _ => out.push_str("There is no plan."),
        }
        out.push_str("\n\n## Files\n\n");
        let list = |want: fn(&CarriedFile) -> bool| {
            self.files
                .iter()
                .filter(|file| want(file))
                .map(|file| format!("`{}`", file.path))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (read, changed) = (list(|file| file.read), list(|file| file.changed));
        if read.is_empty() && changed.is_empty() {
            out.push_str("No file has been read or changed.");
        } else {
            let _ = write!(
                out,
                "Read: {}\nChanged: {}",
                if read.is_empty() { "none" } else { &read },
                if changed.is_empty() { "none" } else { &changed }
            );
        }
        out.push_str("\n\n## The user's last message\n\n");
        match &self.last_user {
            Some(parts) => {
                let mut first = true;
                for part in parts {
                    if !first {
                        out.push_str("\n\n");
                    }
                    first = false;
                    match part {
                        Part::Text(text) => out.push_str(&text.text),
                        Part::Image(image) => {
                            let _ = write!(
                                out,
                                "[an image the user attached{}]",
                                image
                                    .artifact
                                    .as_ref()
                                    .map(|id| format!(", stored as `{id}`"))
                                    .unwrap_or_default()
                            );
                        }
                        _ => {}
                    }
                }
            }
            None => out.push_str("There is none."),
        }
        if !self.artifacts.is_empty() {
            out.push_str("\n\n## Artifacts of the last rounds\n");
            for artifact in &self.artifacts {
                let _ = write!(out, "\n- `{}`: {}", artifact.id, artifact.label);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::ids::PlanItemId;
    use cortex_protocol::item::PlanItem;

    use super::*;

    #[test]
    fn the_carried_state_is_in_the_text_and_in_the_data() {
        let carried = Carried {
            plan: Some(Plan {
                items: vec![
                    PlanItem {
                        id: PlanItemId::new("p1"),
                        text: "Read the code".to_owned(),
                        status: PlanStatus::Done,
                    },
                    PlanItem {
                        id: PlanItemId::new("p2"),
                        text: "Fix the bug".to_owned(),
                        status: PlanStatus::Active,
                    },
                ],
                revision: 4,
            }),
            files: vec![CarriedFile {
                path: "src/lib.rs".to_owned(),
                read: true,
                changed: true,
                hash: Some("ab".to_owned()),
            }],
            last_user: Some(vec![Part::text("Fix the off-by-one in `count`, please.")]),
            artifacts: vec![ArtifactRef {
                id: cortex_protocol::ArtifactId::new("a_0003"),
                label: "output of cargo test".to_owned(),
                bytes: Some(10),
            }],
        };
        let text = carried.render("Goal: fix it.", "2026-11-14");
        assert!(text.contains("Today's date is 2026-11-14 (UTC)."));
        assert!(text.contains("- [done] Read the code (p1)\n- [active] Fix the bug (p2)"));
        assert!(text.contains("Read: `src/lib.rs`\nChanged: `src/lib.rs`"));
        assert!(text.contains("Fix the off-by-one in `count`, please."));
        assert!(text.contains("`a_0003`: output of cargo test"));
        let value = carried.to_value();
        assert_eq!(value["plan"]["revision"], 4);
        assert_eq!(value["files"][0]["hash"], "ab");
        assert_eq!(
            value["last_user"][0]["text"],
            "Fix the off-by-one in `count`, please."
        );
    }

    #[test]
    fn the_focus_follows_the_instructions() {
        let message = instructions_message(Some("  the failing test  "));
        let Part::Text(text) = &message.parts[0] else {
            panic!("text")
        };
        assert!(text.text.starts_with(NOTICE_LEAD));
        assert!(text.text.ends_with("keep in particular: the failing test"));
        let Part::Text(plain) = &instructions_message(Some(" ")).parts[0] else {
            panic!("text")
        };
        assert!(plain.text.ends_with("no longer matters."));
    }
}
