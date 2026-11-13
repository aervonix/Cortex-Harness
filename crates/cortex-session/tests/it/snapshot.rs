//! Per-turn snapshots, undo and rewind of files.

use std::collections::BTreeMap;
use std::fs;

use cortex_base::{AbsPath, ContentHash, PathStyle};
use cortex_protocol::ids::{SessionId, TurnId};
use cortex_session::entry::{Before, Body, EntryId, FileVersion};
use cortex_session::{FileStatus, KEEP_LIMIT, Restore, RoundCheckpoint, Session, SessionError};
use cortex_testkit::cases::{self, ensure};
use cortex_testkit::{Rng, SeededRng};

use crate::{World, user};

/// A tool writing a file, through a round's checkpoint.
fn tool_write(
    world: &World,
    round: &mut RoundCheckpoint,
    session: &Session,
    path: &str,
    content: Option<&[u8]>,
) {
    let file = world.abs(path);
    round.before(&session.store(), &file).unwrap();
    match content {
        Some(content) => {
            world.write(path, content);
            round.after(
                &file,
                Some(FileVersion {
                    hash: ContentHash::of(content),
                    bytes: content.len() as u64,
                    kept: true,
                }),
            );
        }
        None => {
            fs::remove_file(world.file(path)).unwrap();
            round.after(&file, None);
        }
    }
}

fn end_round(session: &mut Session, turn: &TurnId, round: RoundCheckpoint) {
    if let Some(body) = round.finish().unwrap() {
        session.append(Some(turn), body).unwrap();
    }
}

#[test]
fn a_turns_snapshot_restores_exactly_including_files_that_did_not_exist() {
    let world = World::new("snapshot_exact");
    world.write("a.txt", b"first\r\nversion\n");
    world.write("b/c.txt", b"to be removed");
    world.write("binary.bin", &[0, 159, 146, 150, 255]);
    let before = world.contents();

    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("change things")).unwrap();
    let mut round = RoundCheckpoint::new(world.root.clone());
    tool_write(&world, &mut round, &session, "a.txt", Some(b"second"));
    tool_write(&world, &mut round, &session, "new/dir/n.txt", Some(b"made"));
    tool_write(&world, &mut round, &session, "b/c.txt", None);
    end_round(&mut session, &turn, round);
    // A second round in the same turn changes a file again.
    let mut round = RoundCheckpoint::new(world.root.clone());
    tool_write(&world, &mut round, &session, "a.txt", Some(b"third"));
    tool_write(
        &world,
        &mut round,
        &session,
        "binary.bin",
        Some(b"text now"),
    );
    end_round(&mut session, &turn, round);
    assert_ne!(world.contents(), before);

    let plan = session.plan_undo().unwrap().unwrap();
    let paths: Vec<&str> = plan.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "new/dir/n.txt", "b/c.txt", "binary.bin"]);
    assert!(
        plan.files
            .iter()
            .all(|file| file.status == FileStatus::AsLeft),
        "{plan:?}"
    );
    let done = session.rewind(&plan, &Restore::files()).unwrap();
    assert!(done.not_restored.is_empty(), "{done:?}");
    let mut after = world.contents();
    // The directory a tool made is left, empty; the file in it is gone.
    assert!(world.file("new/dir").is_dir());
    after.retain(|path, _| !path.starts_with("new/"));
    assert_eq!(after, before);
}

#[test]
fn a_file_edited_since_is_overwritten_only_when_the_user_says_so() {
    let world = World::new("snapshot_edited");
    world.write("a.txt", b"original");
    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("edit")).unwrap();
    let mut round = RoundCheckpoint::new(world.root.clone());
    tool_write(&world, &mut round, &session, "a.txt", Some(b"by the agent"));
    end_round(&mut session, &turn, round);
    world.write("a.txt", b"by the user");

    let plan = session.plan_undo().unwrap().unwrap();
    assert_eq!(plan.edited_since().len(), 1);
    let done = session.rewind(&plan, &Restore::files()).unwrap();
    assert_eq!(done.not_restored.len(), 1);
    assert_eq!(fs::read(world.file("a.txt")).unwrap(), b"by the user");

    // Back on the turn's branch, and this time the user agrees.
    session.switch(&plan.from.clone().unwrap()).unwrap();
    let plan = session.plan_undo().unwrap().unwrap();
    let done = session
        .rewind(
            &plan,
            &Restore {
                files: true,
                overwrite_edited: vec!["a.txt".into()],
            },
        )
        .unwrap();
    assert_eq!(done.restored, ["a.txt"]);
    assert_eq!(fs::read(world.file("a.txt")).unwrap(), b"original");
}

#[test]
fn a_file_too_large_to_keep_is_named_before_the_rewind() {
    let world = World::new("snapshot_too_large");
    world.write("big.dat", &[7u8; 64]);
    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("shrink it")).unwrap();
    let mut round = RoundCheckpoint::with_limit(world.root.clone(), 32);
    tool_write(&world, &mut round, &session, "big.dat", Some(b"small"));
    end_round(&mut session, &turn, round);
    let checkpoint = session.read(session.active().unwrap()).unwrap();
    let Body::Checkpoint(checkpoint) = checkpoint.body else {
        panic!()
    };
    let before = *checkpoint.files[0].before.version().unwrap();
    assert!(!before.kept);
    assert_eq!(before.bytes, 64);
    assert_eq!(before.hash, ContentHash::of(&[7u8; 64]));
    assert!(!session.store().has_blob(&before.hash), "not copied");

    let plan = session.plan_undo().unwrap().unwrap();
    assert_eq!(plan.cannot_restore().len(), 1);
    let done = session.rewind(&plan, &Restore::files()).unwrap();
    assert_eq!(done.not_restored.len(), 1);
    assert_eq!(fs::read(world.file("big.dat")).unwrap(), b"small");
}

#[test]
fn a_round_that_changes_nothing_records_nothing() {
    let world = World::new("snapshot_nothing");
    world.write("a.txt", b"same");
    let session = world.create("s_1");
    let mut round = RoundCheckpoint::new(world.root.clone());
    round.before(&session.store(), &world.abs("a.txt")).unwrap();
    round
        .before(&session.store(), &world.abs("missing.txt"))
        .unwrap();
    assert!(round.finish().unwrap().is_none());
}

/// One round's writes: a file by its number, and the byte it is filled
/// with, or `None` to remove it.
type Round = Vec<(usize, Option<u8>)>;

#[test]
fn rewinding_to_any_turn_puts_every_file_back_as_it_was_then() {
    let names = ["a", "b", "d/c"];
    cases::check(
        "rewind_restores",
        25,
        |rng| {
            // Turns of rounds of writes (Some) and removals (None).
            let turns: Vec<Vec<Round>> = (0..1 + cases::small(rng, 4))
                .map(|_| {
                    (0..1 + cases::small(rng, 2))
                        .map(|_| {
                            (0..1 + cases::small(rng, 3))
                                .map(|_| {
                                    let name = rng.below(names.len() as u64) as usize;
                                    let content = (rng.below(4) != 0).then(|| rng.below(5) as u8);
                                    (name, content)
                                })
                                .collect()
                        })
                        .collect()
                })
                .collect();
            let back_to = rng.below(turns.len() as u64) as usize;
            (turns, back_to, rng.next_u64())
        },
        |(turns, back_to, seed)| {
            let world = World::new("rewind_property");
            let mut rng = SeededRng::new(*seed);
            for name in names {
                if rng.below(2) == 0 {
                    world.write(name, name.as_bytes());
                }
            }
            let mut session = world.create("s_1");
            let mut states = Vec::new();
            let mut starts = Vec::new();
            for (number, rounds) in turns.iter().enumerate() {
                states.push(world.contents());
                let turn = TurnId::new(format!("t_{}", number + 1));
                starts.push(session.append(Some(&turn), user("next")).unwrap());
                for round_writes in rounds {
                    let mut round = RoundCheckpoint::new(world.root.clone());
                    for &(name, content) in round_writes {
                        let path = names[name];
                        let exists = world.file(path).exists();
                        match content {
                            Some(byte) => {
                                tool_write(&world, &mut round, &session, path, Some(&[byte; 3]));
                            }
                            None if exists => tool_write(&world, &mut round, &session, path, None),
                            None => {}
                        }
                    }
                    end_round(&mut session, &turn, round);
                }
            }
            let target = session.parent_of(&starts[*back_to]).unwrap().clone();
            let plan = session.plan_rewind(&target)?;
            let done = session.rewind(&plan, &Restore::files())?;
            ensure!(
                done.not_restored.is_empty(),
                "not restored: {:?}",
                done.not_restored
            );
            let now: BTreeMap<String, Vec<u8>> = world.contents();
            let then = &states[*back_to];
            ensure!(&now == then, "the workspace is {now:?}, and was {then:?}");
            Ok(())
        },
    );
}

fn version_of(content: &[u8]) -> FileVersion {
    FileVersion {
        hash: ContentHash::of(content),
        bytes: content.len() as u64,
        kept: true,
    }
}

/// A command changed a file that existed, and what it held before could
/// not be recovered: only what the command left is recorded. The review
/// of step 8 found that an undo then deleted the file and said it had put
/// it back.
#[test]
fn undo_never_removes_a_file_whose_earlier_content_is_unknown() {
    let world = World::new("snapshot_before_unknown");
    world.write("keep.txt", b"precious, existed before the turn");
    world.write("gone.txt", b"also existed");
    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("run the script")).unwrap();
    let left = b"changed by a command";
    world.write("keep.txt", left);
    fs::remove_file(world.file("gone.txt")).unwrap();
    let mut round = RoundCheckpoint::new(world.root.clone());
    round.after(&world.abs("keep.txt"), Some(version_of(left)));
    // Removed by the command: recorded too, not taken for "no change".
    round.after(&world.abs("gone.txt"), None);
    end_round(&mut session, &turn, round);

    let plan = session.plan_undo().unwrap().unwrap();
    let paths: Vec<&str> = plan.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["keep.txt", "gone.txt"]);
    for file in &plan.files {
        assert_eq!(file.restore, Before::Unknown, "{file:?}");
        assert_eq!(file.status, FileStatus::CannotRestore, "{file:?}");
        assert!(file.why_not().unwrap().contains("not recorded"), "{file:?}");
    }
    assert_eq!(plan.cannot_restore().len(), 2);

    // A file the user puts back by hand is not removed either.
    world.write("gone.txt", b"put back by the user");
    let done = session.rewind(&plan, &Restore::files()).unwrap();
    assert!(done.restored.is_empty(), "{done:?}");
    assert_eq!(done.not_restored.len(), 2, "{done:?}");
    assert_eq!(fs::read(world.file("keep.txt")).unwrap(), left);
    assert_eq!(
        fs::read(world.file("gone.txt")).unwrap(),
        b"put back by the user"
    );
}

/// What step 13 does with a shell command's changes: a created file is
/// known not to have existed, a file whose earlier content the repository
/// still has is stored from that content, and any other change is recorded
/// as unknown.
#[test]
fn a_commands_changes_are_undone_as_far_as_what_was_before_is_known() {
    let world = World::new("snapshot_command");
    world.write("tracked.rs", b"fn old() {}\n");
    world.write("edited-before.rs", b"edited before the command");
    let mut session = world.create("s_1");
    let store = session.store();
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("run the build")).unwrap();
    // The command.
    world.write("created.txt", b"made by the command");
    world.write("tracked.rs", b"fn new() {}\n");
    world.write("edited-before.rs", b"changed again");

    let mut round = RoundCheckpoint::new(world.root.clone());
    round.record_before(&world.abs("created.txt"), Before::Absent);
    // As the repository would give it, from the commit before the command.
    let from_repository = store
        .put_stream(&mut &b"fn old() {}\n"[..], round.limit())
        .unwrap();
    round.record_before(&world.abs("tracked.rs"), Before::Version(from_repository));
    round.record_before(&world.abs("edited-before.rs"), Before::Unknown);
    // A later record of a file in the same round does not replace the
    // first.
    round.record_before(&world.abs("tracked.rs"), Before::Unknown);
    end_round(&mut session, &turn, round);

    let plan = session.plan_undo().unwrap().unwrap();
    let statuses: Vec<(&str, FileStatus)> = plan
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status))
        .collect();
    assert_eq!(
        statuses,
        [
            ("created.txt", FileStatus::AsLeft),
            ("tracked.rs", FileStatus::AsLeft),
            ("edited-before.rs", FileStatus::CannotRestore)
        ]
    );
    let done = session.rewind(&plan, &Restore::files()).unwrap();
    assert_eq!(done.restored, ["created.txt", "tracked.rs"]);
    assert!(!world.file("created.txt").exists());
    assert_eq!(
        fs::read(world.file("tracked.rs")).unwrap(),
        b"fn old() {}\n"
    );
    assert_eq!(
        fs::read(world.file("edited-before.rs")).unwrap(),
        b"changed again"
    );
}

#[test]
fn content_from_elsewhere_keeps_to_the_same_limit() {
    let world = World::new("snapshot_stream_limit");
    let session = world.create("s_1");
    let store = session.store();
    let round = RoundCheckpoint::with_limit(world.root.clone(), 4);
    let big = store
        .put_stream(&mut &b"too big"[..], round.limit())
        .unwrap();
    assert_eq!(
        big,
        FileVersion {
            kept: false,
            ..version_of(b"too big")
        }
    );
    assert!(!store.has_blob(&big.hash));
    let small = store.put_stream(&mut &b"tiny"[..], KEEP_LIMIT).unwrap();
    assert_eq!(small, version_of(b"tiny"));
    assert_eq!(store.get_blob(&small.hash).unwrap().unwrap(), b"tiny");
}

/// On Windows `Src/A.rs` and `src/a.rs` are one file: its first `before`
/// is what the round started from, whichever spelling a later tool used.
#[test]
fn two_spellings_of_one_file_are_one_change() {
    let root = AbsPath::parse("C:/work", PathStyle::Windows).unwrap();
    let mut round = RoundCheckpoint::new(root.clone());
    let first = root.join("Src/A.rs").unwrap();
    let second = root.join("src/a.rs").unwrap();
    round.record_before(&first, Before::Version(version_of(b"one")));
    round.record_before(&second, Before::Absent);
    round.after(&second, Some(version_of(b"two")));
    let Some(Body::Checkpoint(checkpoint)) = round.finish().unwrap() else {
        panic!("the round changed a file")
    };
    assert_eq!(checkpoint.files.len(), 1, "{checkpoint:?}");
    let file = &checkpoint.files[0];
    assert_eq!(file.path, "Src/A.rs");
    assert_eq!(file.before, Before::Version(version_of(b"one")));
    assert_eq!(file.after, Some(version_of(b"two")));
}

#[test]
fn a_rewind_follows_only_a_plan_made_for_the_session_where_it_is() {
    let world = World::new("snapshot_plan_checked");
    world.write("a.txt", b"original");
    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("edit")).unwrap();
    let mut round = RoundCheckpoint::new(world.root.clone());
    tool_write(&world, &mut round, &session, "a.txt", Some(b"edited"));
    end_round(&mut session, &turn, round);
    let plan = session.plan_undo().unwrap().unwrap();
    let invalid = |result: Result<_, SessionError>| matches!(result, Err(SessionError::Invalid(_)));

    // A target that is not an entry, or is a record.
    let mut unknown = plan.clone();
    unknown.to = EntryId::new("e_0099");
    assert!(invalid(session.rewind(&unknown, &Restore::files())));
    let record = session
        .append(
            Some(&turn),
            Body::Meta(cortex_session::entry::Meta {
                title: Some("t".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    let mut to_record = plan.clone();
    to_record.to = record;
    assert!(invalid(session.rewind(&to_record, &Restore::files())));
    // What it would put back, changed after it was made.
    let mut changed = plan.clone();
    changed.files[0].restore = Before::Absent;
    assert!(invalid(session.rewind(&changed, &Restore::files())));
    changed.files.clear();
    assert!(invalid(session.rewind(&changed, &Restore::files())));
    // A plan made for another session, at the same entry ids.
    let (mut fork, _) = session
        .fork(None, SessionId::new("s_2"), world.options())
        .unwrap();
    assert!(invalid(fork.rewind(&plan, &Restore::files())));
    // None of that moved anything or touched a file.
    assert_eq!(fs::read(world.file("a.txt")).unwrap(), b"edited");
    assert_eq!(session.active(), plan.from.as_ref());

    // The plan itself is followed, once.
    let done = session.rewind(&plan, &Restore::files()).unwrap();
    assert_eq!(done.restored, ["a.txt"]);
    assert_eq!(fs::read(world.file("a.txt")).unwrap(), b"original");
    assert!(invalid(session.rewind(&plan, &Restore::files())));
}
