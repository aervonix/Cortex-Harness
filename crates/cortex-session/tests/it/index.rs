//! Listing sessions, and finding one by a prefix of its id.

use std::fs;
use std::time::Duration;

use cortex_protocol::ids::SessionId;
use cortex_protocol::session::SessionSummary;
use cortex_session::SessionError;
use cortex_session::entry::{Body, Meta};

use crate::{World, user};

fn make(world: &World, id: &str, title: &str) {
    let mut session = world.create(id);
    session
        .append(None, user(&format!("prompt of {id}")))
        .unwrap();
    session
        .append(
            None,
            Body::Meta(Meta {
                title: Some(title.into()),
                ..Meta::default()
            }),
        )
        .unwrap();
    session.close().unwrap();
    world.clock.advance(Duration::from_millis(250));
}

#[test]
fn sessions_are_listed_newest_first_from_the_index() {
    let world = World::new("index_list");
    make(&world, "s_01a", "first");
    make(&world, "s_01b", "second");
    make(&world, "s_02c", "third");
    let listed = world.project.index().list().unwrap();
    let ids: Vec<&str> = listed.iter().map(|record| record.id.as_str()).collect();
    assert_eq!(ids, ["s_02c", "s_01b", "s_01a"]);
    assert_eq!(listed[0].title.as_deref(), Some("third"));
    assert_eq!(listed[0].first_prompt.as_deref(), Some("prompt of s_02c"));
    assert_eq!(listed[0].entries, 3);
    let summary = SessionSummary::from(&listed[0]);
    assert_eq!(summary.session_id.as_str(), "s_02c");
    assert_eq!(
        world.project.index().latest().unwrap().unwrap().id.as_str(),
        "s_02c"
    );
}

#[test]
fn a_missing_or_stale_index_is_put_right_from_the_logs() {
    let world = World::new("index_stale");
    make(&world, "s_1", "one");
    make(&world, "s_2", "two");
    let index = world.project.index();
    fs::remove_file(world.project.path().join("index.json")).unwrap();
    assert_eq!(index.list().unwrap().len(), 2, "rebuilt");

    fs::write(world.project.path().join("index.json"), b"{ not an index").unwrap();
    assert_eq!(index.list().unwrap().len(), 2, "rebuilt");

    // A session the index does not know (its writer crashed before the
    // index was updated), and one whose directory is gone.
    let saved = fs::read(world.project.path().join("index.json")).unwrap();
    make(&world, "s_3", "three");
    fs::write(world.project.path().join("index.json"), saved).unwrap();
    fs::remove_dir_all(world.project.path().join("s_1")).unwrap();
    let ids: Vec<String> = index
        .list()
        .unwrap()
        .into_iter()
        .map(|record| record.id.to_string())
        .collect();
    assert_eq!(ids, ["s_3", "s_2"]);
    let text = fs::read_to_string(world.project.path().join("index.json")).unwrap();
    assert!(
        text.contains("s_3") && !text.contains("s_1"),
        "the fix is written back"
    );
}

#[test]
fn a_unique_prefix_finds_a_session() {
    let world = World::new("index_find");
    make(&world, "s_01abc", "a");
    make(&world, "s_01abd", "b");
    make(&world, "s_02xyz", "c");
    let index = world.project.index();
    assert_eq!(index.find("s_02").unwrap(), SessionId::new("s_02xyz"));
    assert_eq!(index.find("s_01abc").unwrap(), SessionId::new("s_01abc"));
    assert!(
        matches!(index.find("s_01ab"), Err(SessionError::AmbiguousPrefix { candidates, .. }) if candidates.len() == 2)
    );
    assert!(matches!(
        index.find("s_9"),
        Err(SessionError::NotFound { .. })
    ));
    assert!(matches!(index.find(""), Err(SessionError::NotFound { .. })));
}

#[test]
fn a_project_with_no_sessions_lists_none() {
    let world = World::new("index_empty");
    assert!(world.project.index().list().unwrap().is_empty());
    assert!(world.project.index().latest().unwrap().is_none());
}
