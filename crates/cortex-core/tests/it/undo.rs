//! Undo of a turn restores every file the turn's tools changed
//! (`docs/architecture/07-sessions-and-context.md`, "Snapshots and undo";
//! `docs/architecture/05-tools.md`, "Checkpoints").
//!
//! The real `write` and `edit` tools run against a real session. What
//! joins them is [`RoundCheckpoints`], a test-only adapter from the tools'
//! checkpoint seam to the session's `RoundCheckpoint`: the core's own
//! adapter, when the core is assembled, does the same and can start from
//! this one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cortex_base::{AbsPath, ContentHash, FakeClock, PathOptions, PathResolver};
use cortex_protocol::ids::{ItemId, SessionId, TurnId};
use cortex_protocol::item::UserMessageItem;
use cortex_protocol::message::Part;
use cortex_session::entry::{Before, Body, FileVersion};
use cortex_session::{
    DiskStore, Durability, FileStatus, ProjectSessions, Restore, RoundCheckpoint, Session,
    SessionOptions,
};
use cortex_testkit::cases::{self, ensure};
use cortex_testkit::{Rng, SeededRng};
use cortex_tools::{
    CheckpointError, Checkpoints, EditTool, Editing, FileSnapshot, FileTable, MemoryStore,
    NoOutput, ReadTool, RunCx, Tool, ToolResult, WriteTool, execute,
};
use serde_json::{Value, json};

/// The tools' checkpoint seam, filled with the checkpoint of the tool round
/// that is running.
struct RoundCheckpoints {
    store: Arc<DiskStore>,
    root: AbsPath,
    round: Mutex<RoundCheckpoint>,
}

impl RoundCheckpoints {
    fn new(store: Arc<DiskStore>, root: &AbsPath) -> Self {
        Self {
            store,
            root: root.clone(),
            round: Mutex::new(RoundCheckpoint::new(root.clone())),
        }
    }

    /// The end of a tool round: its `checkpoint` entry, if a file changed,
    /// and a fresh round for the next.
    fn finish(&self) -> Option<Body> {
        let round = std::mem::replace(
            &mut *self.round.lock().unwrap(),
            RoundCheckpoint::new(self.root.clone()),
        );
        round.finish().expect("the round's files can be read")
    }
}

impl Checkpoints for RoundCheckpoints {
    fn before(&self, file: &AbsPath, content: Option<&[u8]>) -> Result<(), CheckpointError> {
        let mut round = self.round.lock().unwrap();
        let before = match content {
            None => Before::Absent,
            // The bytes the tool read are what is kept: no second read of
            // the file, and nothing between the two for it to become.
            Some(content) => {
                let bytes = content.len() as u64;
                let kept = bytes <= round.limit();
                let hash = if kept {
                    self.store
                        .put_blob(content)
                        .map_err(|error| CheckpointError::new(error.to_string()))?
                } else {
                    ContentHash::of(content)
                };
                Before::Version(FileVersion { hash, bytes, kept })
            }
        };
        // Only the first record of a file in a round counts.
        round.record_before(file, before);
        Ok(())
    }

    fn after(&self, file: &AbsPath, left: &FileSnapshot) {
        self.round.lock().unwrap().after(
            file,
            Some(FileVersion {
                hash: left.hash,
                bytes: left.size,
                kept: true,
            }),
        );
    }
}

/// A workspace, a session in it, and the tools wired to that session.
struct World {
    dir: PathBuf,
    workspace: PathBuf,
    session: Session,
    checkpoints: Arc<RoundCheckpoints>,
    cx: RunCx,
    read: ReadTool,
    write: WriteTool,
    edit: EditTool,
    runtime: tokio::runtime::Runtime,
    turns: u32,
}

impl World {
    fn new(test: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("cortex-core-it")
            .join(test);
        if dir.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
        let workspace = dir.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let paths = Arc::new(PathResolver::new(&workspace, PathOptions::new()).unwrap());
        let root = paths.root().clone();
        let clock = Arc::new(FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap());
        let project = ProjectSessions::at(dir.join("sessions").join(root.project_key().as_str()));
        let session = Session::create(
            &project,
            SessionId::new("s_1"),
            &root,
            SessionOptions {
                durability: Durability::Never,
                ..SessionOptions::new(clock.clone(), "0.0.0-test")
            },
        )
        .unwrap();
        let checkpoints = Arc::new(RoundCheckpoints::new(session.store(), &root));
        // No flush to the disk, which takes as long as the disk takes and is
        // not what these tests check. The measurement can ask for it.
        let sync = std::env::var_os("CORTEX_PERF_SYNC").is_some();
        let editing = Editing::new(checkpoints.clone(), clock).with_sync(sync);
        Self {
            cx: RunCx::new(
                paths,
                Arc::new(MemoryStore::new()),
                Arc::new(FileTable::new()),
            ),
            read: ReadTool::new(),
            write: WriteTool::new(editing.clone()),
            edit: EditTool::new(editing),
            runtime: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
            dir,
            workspace,
            session,
            checkpoints,
            turns: 0,
        }
    }

    fn put(&self, relative: &str, content: &[u8]) {
        let path = self.workspace.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Every file of the workspace and its content.
    fn contents(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, prefix: &str, found: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let name = entry.file_name().into_string().unwrap();
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), &path, found);
                } else {
                    found.insert(path, fs::read(entry.path()).unwrap());
                }
            }
        }
        let mut found = BTreeMap::new();
        walk(&self.workspace, "", &mut found);
        found
    }

    /// Plans and runs one call, as the loop does.
    fn call(&self, tool: &dyn Tool, args: Value) -> ToolResult {
        match tool.plan(&args, &self.cx.plan_cx()) {
            Ok(plan) => self
                .runtime
                .block_on(execute(tool, plan, &self.cx, &mut NoOutput)),
            Err(error) => error.into_result(),
        }
    }

    fn read(&self, path: &str) {
        let result = self.call(&self.read, json!({"path": path, "fresh": true}));
        assert!(!result.is_error(), "{}", result.text());
    }

    fn write(&self, path: &str, content: &str) -> ToolResult {
        self.call(&self.write, json!({"path": path, "content": content}))
    }

    fn edit(&self, path: &str, old: &str, new: &str) -> ToolResult {
        self.call(
            &self.edit,
            json!({"path": path, "edits": [{"old": old, "new": new}]}),
        )
    }

    /// Starts a turn: the user's prompt goes into the log.
    fn begin_turn(&mut self) -> TurnId {
        self.turns += 1;
        let turn = TurnId::new(format!("t_{}", self.turns));
        let prompt = Body::User(UserMessageItem {
            id: ItemId::new(format!("i_{}", self.turns)),
            parts: vec![Part::text("change things")],
            delivery: None,
            created_at: None,
        });
        self.session.append(Some(&turn), prompt).unwrap();
        turn
    }

    /// Ends a tool round: its checkpoint goes into the log.
    fn end_round(&mut self, turn: &TurnId) -> bool {
        match self.checkpoints.finish() {
            Some(body) => {
                self.session.append(Some(turn), body).unwrap();
                true
            }
            None => false,
        }
    }

    /// Undoes the last turn, putting its files back.
    fn undo(&mut self) -> cortex_session::RewindDone {
        let plan = self.session.plan_undo().unwrap().expect("a turn to undo");
        assert!(
            plan.files.iter().all(|file| matches!(
                file.status,
                FileStatus::AsLeft | FileStatus::AlreadyRestored
            )),
            "every file is as the tools left it: {plan:?}"
        );
        self.session.rewind(&plan, &Restore::files()).unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[track_caller]
fn ok(result: &ToolResult) {
    assert!(!result.is_error(), "the call failed: {}", result.text());
}

#[test]
fn undo_of_a_turn_restores_every_file_the_turns_tools_changed() {
    let mut world = World::new("undo_turn");
    world.put("src/main.rs", b"fn main() {\n    println!(\"hi\");\n}\n");
    // A mark, CRLF endings, and no line break at the end.
    world.put("notes/win.txt", b"\xef\xbb\xbfone\r\ntwo\r\nthree");
    world.put("mixed.txt", b"a\r\nb\nc\r\n");
    world.put("untouched.txt", b"left alone\n");
    let before = world.contents();
    for path in ["src/main.rs", "notes/win.txt", "mixed.txt"] {
        world.read(path);
    }

    let turn = world.begin_turn();
    // Round one: an edit, a new file in a new directory, a whole rewrite.
    ok(&world.edit("src/main.rs", "\"hi\"", "\"hello\""));
    ok(&world.write("docs/new/guide.md", "# Guide\n\nNew.\n"));
    ok(&world.write("notes/win.txt", "1\n2\n"));
    assert!(world.end_round(&turn), "the round changed files");
    // Round two: the same file again, the new file, and one more.
    ok(&world.edit("src/main.rs", "fn main()", "pub fn main()"));
    ok(&world.edit("docs/new/guide.md", "New.", "Newer."));
    ok(&world.edit("mixed.txt", "b", "B\nB2"));
    // A call that fails changes nothing, and adds nothing to the round.
    assert!(world.edit("mixed.txt", "no such text", "x").is_error());
    assert!(world.end_round(&turn));
    // A round that changes nothing costs nothing.
    world.read("untouched.txt");
    assert!(!world.end_round(&turn));

    let changed = world.contents();
    assert_eq!(
        changed["src/main.rs"],
        b"pub fn main() {\n    println!(\"hello\");\n}\n"
    );
    assert_eq!(changed["notes/win.txt"], b"\xef\xbb\xbf1\r\n2\r\n");
    assert_eq!(changed["mixed.txt"], b"a\r\nB\nB2\nc\r\n");
    assert_eq!(changed["docs/new/guide.md"], b"# Guide\n\nNewer.\n");

    let done = world.undo();
    assert!(done.not_restored.is_empty(), "{done:?}");
    let mut restored = done.restored.clone();
    restored.sort();
    assert_eq!(
        restored,
        [
            "docs/new/guide.md",
            "mixed.txt",
            "notes/win.txt",
            "src/main.rs"
        ]
    );
    // Byte for byte, and the file the turn created is gone.
    assert_eq!(world.contents(), before);
}

#[test]
fn undo_puts_back_the_state_before_the_turn_and_not_before_an_earlier_one() {
    let mut world = World::new("undo_last_turn");
    world.put("a.txt", b"zero\n");
    world.read("a.txt");

    let first = world.begin_turn();
    ok(&world.edit("a.txt", "zero", "one"));
    world.end_round(&first);
    let after_first = world.contents();

    let second = world.begin_turn();
    ok(&world.edit("a.txt", "one", "two"));
    ok(&world.write("b.txt", "made in turn two\n"));
    world.end_round(&second);

    world.undo();
    assert_eq!(world.contents(), after_first, "turn one's work stays");
}

#[test]
fn a_file_the_user_edited_after_the_turn_is_not_overwritten_by_undo() {
    let mut world = World::new("undo_edited_since");
    world.put("a.txt", b"original\n");
    world.read("a.txt");
    let turn = world.begin_turn();
    ok(&world.edit("a.txt", "original", "by the agent"));
    world.end_round(&turn);
    world.put("a.txt", b"by the user\n");

    let plan = world.session.plan_undo().unwrap().unwrap();
    assert_eq!(plan.edited_since().len(), 1);
    let done = world.session.rewind(&plan, &Restore::files()).unwrap();
    assert!(done.restored.is_empty());
    assert_eq!(world.contents()["a.txt"], b"by the user\n");
}

/// What one step of a generated turn does.
#[derive(Debug, Clone)]
enum Step {
    Write { file: usize, lines: u64 },
    Edit { file: usize, line: u64, breaks: u64 },
    EndRound,
}

const NAMES: [&str; 4] = ["a.txt", "src/b.rs", "docs/c.md", "new/d.txt"];

#[test]
fn property_undo_restores_the_workspace_whatever_the_tools_did() {
    cases::check(
        "undo_restores_the_workspace",
        25,
        |rng: &mut SeededRng| {
            // Three files to start with, with either kind of line ending.
            let files: Vec<String> = (0..3)
                .map(|file| {
                    let ending = if rng.below(2) == 0 { "\n" } else { "\r\n" };
                    (0..rng.below(5) + 1)
                        .map(|line| format!("file {file} line {line}{ending}"))
                        .collect()
                })
                .collect();
            let steps: Vec<Step> = (0..rng.below(8) + 1)
                .map(|_| match rng.below(5) {
                    0 => Step::EndRound,
                    1 | 2 => Step::Write {
                        file: rng.below(4) as usize,
                        lines: rng.below(4),
                    },
                    _ => Step::Edit {
                        file: rng.below(3) as usize,
                        line: rng.below(5),
                        breaks: rng.below(2),
                    },
                })
                .collect();
            (files, steps)
        },
        |(files, steps)| {
            let mut world = World::new("undo_property");
            for (name, content) in NAMES.iter().zip(files) {
                world.put(name, content.as_bytes());
                world.read(name);
            }
            let before = world.contents();
            let turn = world.begin_turn();
            let mut changed = false;
            for step in steps {
                match step {
                    Step::Write { file, lines } => {
                        let content: String = (0..*lines)
                            .map(|line| format!("rewritten {line}\n"))
                            .collect();
                        // The fourth name is a file that does not exist
                        // until a step creates it.
                        changed |= !world.write(NAMES[*file], &content).is_error();
                    }
                    Step::Edit { file, line, breaks } => {
                        let old = format!("file {file} line {line}");
                        let new = format!("edited{}", "\nand more".repeat(*breaks as usize));
                        // An edit may fail: of text an earlier step
                        // rewrote. Then it changed nothing.
                        changed |= !world.edit(NAMES[*file], &old, &new).is_error();
                    }
                    Step::EndRound => {
                        world.end_round(&turn);
                    }
                }
            }
            world.end_round(&turn);
            ensure!(
                changed == (world.contents() != before),
                "a call succeeded and nothing changed, or the reverse"
            );

            let done = world.undo();
            ensure!(
                done.not_restored.is_empty(),
                "not restored: {:?}",
                done.not_restored
            );
            let after = world.contents();
            ensure!(
                after == before,
                "after undo: {:?}\n  before the turn: {:?}",
                after.keys().collect::<Vec<_>>(),
                before.keys().collect::<Vec<_>>()
            );
            Ok(())
        },
    );
}

/// What one change costs on the round's path, by the size of the file: the
/// tool call itself (read, hash, match, checkpoint, atomic write), the end
/// of the round, and the undo.
///
/// A measurement, not a blocking test. It is ignored by default and run by
/// hand, in a release build:
///
/// ```text
/// cargo test --release -p cortex-core --test it perf_ -- --ignored --nocapture
/// ```
///
/// With `CORTEX_PERF_SYNC` set, the tools flush each write to the disk, as
/// they do in a session. The numbers it prints are in
/// `docs/performance-budgets.md`.
#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn perf_editing_and_undoing_a_file() {
    use std::time::Instant;

    fn median(mut times: Vec<f64>) -> f64 {
        times.sort_by(f64::total_cmp);
        times[times.len() / 2]
    }
    let millis = |start: Instant| start.elapsed().as_secs_f64() * 1000.0;

    for (label, bytes, runs) in [
        ("10 KiB", 10 * 1024, 40),
        ("1 MiB", 1024 * 1024, 20),
        ("16 MiB (the limit)", 16 * 1024 * 1024, 7),
    ] {
        let mut world = World::new("perf_edit");
        // Lines of 64 bytes, with one marked line in the middle.
        let line = format!("{}\n", "x".repeat(63));
        let count = bytes / line.len();
        let mut content = line.repeat(count / 2);
        content.push_str("the line that is edited, in the middle of the file\n");
        content.push_str(&line.repeat(count / 2));
        content.truncate(bytes);
        world.put("big.txt", content.as_bytes());

        let (mut reads, mut edits, mut rounds, mut undos) = (vec![], vec![], vec![], vec![]);
        for run in 0..runs {
            let start = Instant::now();
            world.read("big.txt");
            reads.push(millis(start));

            let turn = world.begin_turn();
            let start = Instant::now();
            let result = world.edit("big.txt", "that is edited", &format!("edited in run {run}"));
            edits.push(millis(start));
            ok(&result);

            let start = Instant::now();
            assert!(world.end_round(&turn));
            rounds.push(millis(start));

            let start = Instant::now();
            let done = world.undo();
            undos.push(millis(start));
            assert_eq!(done.restored, ["big.txt"]);
        }
        println!(
            "{label}: read {:.2} ms, edit {:.2} ms, end of round {:.3} ms, undo {:.2} ms \
             (medians of {runs})",
            median(reads),
            median(edits),
            median(rounds),
            median(undos),
        );
    }
}
