//! Every file and directory the session store creates is owner-only, on
//! Linux and on Windows (`docs/decisions/0029-owner-only-files-on-windows.md`).

use std::fs;
use std::path::{Path, PathBuf};

use cortex_base::private;
use cortex_protocol::ids::{SessionId, TurnId};
use cortex_session::RoundCheckpoint;

use crate::{World, user};

/// Every file and directory under `root`, `root` included.
fn everything(root: &Path) -> Vec<PathBuf> {
    let mut found = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
            found.push(entry.path());
        }
    }
    found
}

#[test]
fn every_file_and_directory_a_session_creates_is_private() {
    let world = World::new("private_files");
    world.write("a.txt", b"before");
    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("hello")).unwrap();
    assert!(
        session.not_private().is_empty(),
        "{:?}",
        session.not_private()
    );
    let store = session.store();
    store
        .put_artifact("output", "text/plain", b"output")
        .unwrap();
    let mut round = RoundCheckpoint::new(world.root.clone());
    round.before(&store, &world.abs("a.txt")).unwrap();
    world.write("a.txt", b"after");
    session
        .append(Some(&turn), round.finish().unwrap().unwrap())
        .unwrap();
    session.end_turn().unwrap();
    // A cut line set aside by a resume.
    drop(session);
    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    let mut log = fs::OpenOptions::new().append(true).open(dir.log()).unwrap();
    std::io::Write::write_all(&mut log, b"{\"v\":1,\"id\":").unwrap();
    drop(log);
    let (session, report) = world.resume("s_1");
    assert!(report.cut_saved_to.is_some());
    assert!(report.not_private.is_empty(), "{:?}", report.not_private);
    // A fork, and the index.
    session
        .fork(None, SessionId::new("s_2"), world.options())
        .unwrap();
    world.project.index().list().unwrap();

    let all = everything(world.project.path());
    let names: Vec<String> = all
        .iter()
        .map(|path| {
            path.strip_prefix(world.project.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    for expected in [
        "s_1/log.jsonl",
        "s_1/lock",
        "index.json",
        "index.lock",
        "s_2/log.jsonl",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} in {names:?}"
        );
    }
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("s_1/artifacts/a_0001"))
    );
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("s_1/checkpoints/"))
    );
    assert!(names.iter().any(|name| name.starts_with("s_1/log.cut-")));
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("s_2/checkpoints/"))
    );
    for path in all {
        assert!(
            private::is_private(&path).unwrap(),
            "{} can be opened by other users",
            path.display()
        );
    }
}
