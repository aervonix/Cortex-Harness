//! Artifacts and stored contents, and the redaction hook.

use std::borrow::Cow;
use std::fs;
use std::io::{Read, Write};
use std::sync::Arc;

use cortex_base::ContentHash;
use cortex_protocol::ids::RawJson;
use cortex_protocol::ids::{ArtifactId, SessionId};
use cortex_protocol::message::{Part, TextPart};
use cortex_session::{DiskStore, Redactor, SessionError, SessionOptions};

use crate::{World, assistant, user};

fn read_all(store: &DiskStore, id: &ArtifactId) -> Vec<u8> {
    let mut content = Vec::new();
    store
        .open_artifact(id)
        .unwrap()
        .read_to_end(&mut content)
        .unwrap();
    content
}

#[test]
fn an_artifact_written_in_pieces_reads_back_whole_and_ids_continue() {
    let world = World::new("artifacts");
    let session = world.create("s_1");
    let store = session.store();
    let mut writer = store
        .begin_artifact("output of a command", "text/plain")
        .unwrap();
    assert_eq!(writer.id().as_str(), "a_0001");
    writer.write_all(b"first ").unwrap();
    writer.write_all(b"second").unwrap();
    let reference = writer.finish().unwrap();
    assert_eq!(reference.id.as_str(), "a_0001");
    assert_eq!(reference.bytes, Some(12));
    assert_eq!(read_all(&store, &reference.id), b"first second");
    assert!(
        store
            .artifact_path(&reference.id)
            .unwrap()
            .ends_with("a_0001.txt")
    );

    // Abandoned: nothing is left, and its number is not given again.
    let mut abandoned = store.begin_artifact("half", "image/png").unwrap();
    abandoned.write_all(b"\x89PNG").unwrap();
    drop(abandoned);
    assert!(store.open_artifact(&ArtifactId::new("a_0002")).is_err());
    let image = store
        .put_artifact("an image", "image/png", b"\x89PNG")
        .unwrap();
    assert_eq!(image.id.as_str(), "a_0003");

    // A new store over the same directory, as after a resume, continues.
    let again = DiskStore::new(
        session.dir().clone(),
        Arc::new(cortex_session::NoRedaction),
        true,
    );
    assert_eq!(
        again
            .put_artifact("x", "text/plain", b"x")
            .unwrap()
            .id
            .as_str(),
        "a_0004"
    );
    assert_eq!(read_all(&again, &image.id), b"\x89PNG");
}

#[test]
fn an_artifact_id_that_could_be_a_path_is_refused() {
    let world = World::new("artifact_ids");
    let session = world.create("s_1");
    let store = session.store();
    store.put_artifact("x", "text/plain", b"x").unwrap();
    for bad in ["../log.jsonl", "a_0001/../x", "a_", "log", "a_0001.txt"] {
        assert!(
            matches!(
                store.open_artifact(&ArtifactId::new(bad)),
                Err(SessionError::NoSuchEntry { .. })
            ),
            "{bad}"
        );
    }
}

#[test]
fn content_is_stored_once_under_its_hash_and_checked_when_read() {
    let world = World::new("blobs");
    let session = world.create("s_1");
    let store = session.store();
    let hash = store.put_blob(b"fn main() {}\n").unwrap();
    assert_eq!(hash, ContentHash::of(b"fn main() {}\n"));
    assert_eq!(store.put_blob(b"fn main() {}\n").unwrap(), hash);
    let directory = session.dir().checkpoints();
    let names: Vec<String> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        names,
        [hash.to_hex()],
        "one file, filed under the hash, no temporaries"
    );
    assert_eq!(store.get_blob(&hash).unwrap().unwrap(), b"fn main() {}\n");
    assert_eq!(store.get_blob(&ContentHash::of(b"other")).unwrap(), None);

    fs::write(directory.join(hash.to_hex()), b"tampered").unwrap();
    assert!(matches!(
        store.get_blob(&hash),
        Err(SessionError::Corrupt { .. })
    ));
}

#[derive(Debug)]
struct MaskHunter;

impl Redactor for MaskHunter {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if text.contains("hunter2") {
            Cow::Owned(text.replace("hunter2", "[redacted]"))
        } else {
            Cow::Borrowed(text)
        }
    }
}

#[test]
fn the_redactor_sees_entries_and_text_artifacts_but_not_raw_blocks_or_contents() {
    let world = World::new("redaction");
    let options = SessionOptions {
        redactor: Arc::new(MaskHunter),
        ..world.options()
    };
    let mut session = cortex_session::Session::create(
        &world.project,
        SessionId::new("s_1"),
        &world.root,
        options,
    )
    .unwrap();
    session
        .append(None, user("my password is hunter2"))
        .unwrap();
    let mut reply = assistant("i_2", "", &[]);
    if let cortex_session::Body::Assistant { item, .. } = &mut reply {
        item.parts = vec![Part::Text(TextPart {
            text: "you said hunter2".into(),
            raw: Some(RawJson::new(r#"{"text":"you said hunter2"}"#)),
        })];
    }
    session.append(None, reply).unwrap();
    let log = fs::read_to_string(session.dir().log()).unwrap();
    assert_eq!(
        log.matches("hunter2").count(),
        1,
        "only the raw block keeps it: {log}"
    );
    assert!(
        log.contains(r#"you said hunter2\"}"#),
        "the raw block is as it was"
    );

    let store = session.store();
    let mut writer = store.begin_artifact("output", "text/plain").unwrap();
    writer.write_all(b"line one hun").unwrap();
    writer.write_all(b"ter2\nline two\n").unwrap();
    let text = writer.finish().unwrap();
    assert_eq!(
        read_all(&store, &text.id),
        b"line one [redacted]\nline two\n"
    );
    let image = store
        .put_artifact("bytes", "image/png", b"hunter2")
        .unwrap();
    assert_eq!(read_all(&store, &image.id), b"hunter2");
    let hash = store.put_blob(b"hunter2").unwrap();
    assert_eq!(
        store.get_blob(&hash).unwrap().unwrap(),
        b"hunter2",
        "file contents are exact"
    );
}
