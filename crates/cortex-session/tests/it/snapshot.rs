//! Per-turn snapshots, undo and rewind of files.

use std::collections::BTreeMap;
use std::fs;

use cortex_base::ContentHash;
use cortex_protocol::ids::TurnId;
use cortex_session::entry::{Body, FileVersion};
use cortex_session::{FileStatus, Restore, RoundCheckpoint, Session};
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
    let before = checkpoint.files[0].before.unwrap();
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
