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

fn redacting_session(world: &World) -> cortex_session::Session {
    let options = SessionOptions {
        redactor: Arc::new(MaskHunter),
        ..world.options()
    };
    cortex_session::Session::create(&world.project, SessionId::new("s_1"), &world.root, options)
        .unwrap()
}

fn has_secret(bytes: &[u8]) -> bool {
    bytes.windows(7).any(|window| window == b"hunter2")
}

/// One byte that is not UTF-8 anywhere in a write used to send every line
/// written with it to the disk unredacted.
#[test]
fn a_text_artifact_with_a_byte_that_is_not_utf8_is_still_redacted() {
    let world = World::new("redact_latin1");
    let session = redacting_session(&world);
    let store = session.store();
    let mut file = store.begin_artifact("output", "text/plain").unwrap();
    file.write_all(b"password=hunter2\nlatin-1 caf\xe9 hunter2\n")
        .unwrap();
    let reference = file.finish().unwrap();
    let content = read_all(&store, &reference.id);
    assert!(
        !has_secret(&content),
        "{}",
        String::from_utf8_lossy(&content)
    );
    assert_eq!(
        content, b"password=[redacted]\nlatin-1 caf\xe9 [redacted]\n",
        "the byte that is not text is kept as it was"
    );
}

/// A long line arriving in pieces from a pipe used to be cut inside a
/// character, and that piece was written unredacted.
#[test]
fn a_long_line_arriving_in_pieces_is_still_redacted() {
    let world = World::new("redact_split");
    let session = redacting_session(&world);
    let store = session.store();
    let mut text = String::from("hunter2  ");
    text.push_str(&"é".repeat(40_000));
    text.push('\n');
    let mut file = store.begin_artifact("output", "text/plain").unwrap();
    for piece in text.as_bytes().chunks(4096) {
        file.write_all(piece).unwrap();
    }
    let reference = file.finish().unwrap();
    let content = read_all(&store, &reference.id);
    assert!(!has_secret(&content), "written unredacted");
    let content = String::from_utf8(content).expect("no character was split");
    assert_eq!(content, text.replace("hunter2", "[redacted]"));
}

/// Runs `work` on a thread of its own and waits for it within a bound, so
/// that a read that waits on a FIFO is a failed test and not a hung one.
#[cfg(unix)]
fn without_waiting<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(work());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("{what} waited on a FIFO nobody writes to"))
}

/// Opening a FIFO to read it waits for a writer that may never come. The
/// store looks at nothing before it opens: the open itself neither waits
/// nor follows a link, so a file that is swapped for a FIFO or a link at
/// the last moment is refused like one that was always there.
#[cfg(unix)]
#[test]
fn sys_a_file_swapped_for_a_fifo_or_a_link_is_neither_waited_on_nor_followed() {
    use cortex_session::RoundCheckpoint;

    let world = World::new("sys_store_fifo");
    let session = world.create("s_1");
    let store = session.store();
    let path = world.file("build.log");
    let file = world.abs("build.log");

    // A round keeps the file as it was, and a tool changes it.
    world.write("build.log", b"as it was");
    let mut round = RoundCheckpoint::new(world.root.clone());
    round.before(&store, &file).unwrap();
    assert!(store.has_blob(&ContentHash::of(b"as it was")));

    // Then the file is swapped for a FIFO that nobody writes to.
    fs::remove_file(&path).unwrap();
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    if !made.is_ok_and(|status| status.success()) {
        assert!(!crate::in_ci(), "mkfifo is needed in CI");
        return;
    }
    let refused = {
        let (store, path) = (Arc::clone(&store), path.clone());
        without_waiting("put_file", move || {
            store.put_file(&path, cortex_session::KEEP_LIMIT)
        })
    };
    assert!(
        matches!(refused, Err(SessionError::NotARegularFile { .. })),
        "{refused:?}"
    );
    // The end of the round looks at what the file is now: it is known as
    // "not a file", and was not read.
    let body = without_waiting("the end of a round", move || round.finish())
        .unwrap()
        .expect("the file changed");
    let cortex_session::entry::Body::Checkpoint(checkpoint) = body else {
        panic!("a checkpoint entry");
    };
    let after = checkpoint.files[0].after.expect("something is there");
    assert!(!after.kept && after.bytes == 0, "{after:?}");

    // And for a link to a file outside the workspace: nothing of the
    // outside file reaches the store.
    fs::remove_file(&path).unwrap();
    let outside = world.scratch.join("outside-secret");
    fs::write(&outside, b"not the workspace's").unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(matches!(
        store.put_file(&path, cortex_session::KEEP_LIMIT),
        Err(SessionError::NotARegularFile { .. })
    ));
    assert!(!store.has_blob(&ContentHash::of(b"not the workspace's")));
}
