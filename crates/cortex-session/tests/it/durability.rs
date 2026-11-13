//! A log a crash cut short, damage, kinds from later versions, and reading
//! while another process writes.

use std::fs;

use cortex_protocol::ids::{SessionId, TurnId};
use cortex_session::entry::{Body, EntryId, ExtRecord, Kind, Meta};
use cortex_session::{Log, Session, SessionError};
use serde_json::json;

use crate::{World, assistant, tool_result, user};

/// A session whose last entry is `last`, and the byte range of that entry's
/// line, newline included.
fn session_ending_with(world: &World, last: Body) -> (Vec<u8>, usize) {
    let mut session = world.create("s_1");
    let turn = TurnId::new("t_1");
    session.append(Some(&turn), user("read the file")).unwrap();
    session
        .append(Some(&turn), assistant("i_2", "Reading it.", &["c_1"]))
        .unwrap();
    session
        .append(Some(&turn), tool_result("i_3", "c_1"))
        .unwrap();
    let before = fs::metadata(session.dir().log()).unwrap().len() as usize;
    session.append(Some(&turn), last).unwrap();
    drop(session);
    let bytes = fs::read(world.project.session(&SessionId::new("s_1")).unwrap().log()).unwrap();
    (bytes, before)
}

/// Cuts the log at every byte of its last entry, `last`, and checks that
/// each cut opens cleanly read-only without being changed, and that a
/// resume repairs it so that the session goes on.
fn cut_at_every_byte(test: &str, last: Body) {
    let world = World::new(test);
    let kind = last.kind();
    let is_record = last.is_record();
    let (bytes, start) = session_ending_with(&world, last);
    let whole = 5; // the header, three entries and the last one
    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    for cut in start..=bytes.len() {
        // The directory is kept from one cut to the next (a new one each
        // time costs more than the test itself on Windows); only what a
        // resume sets aside is removed.
        fs::write(dir.log(), &bytes[..cut]).unwrap();
        let what = format!(
            "{} cut at {} of {}",
            kind.as_str(),
            cut - start,
            bytes.len() - start
        );

        // Read-only: opens, reads every entry, changes nothing.
        let (log, report) = Log::open(&dir).unwrap_or_else(|error| panic!("{what}: {error}"));
        let survived = cut >= bytes.len() - 1;
        assert_eq!(
            log.len(),
            if survived { whole } else { whole - 1 },
            "{what}"
        );
        assert_eq!(
            report.cut_tail.is_some(),
            cut > start && !survived,
            "{what}"
        );
        assert!(report.damaged_lines.is_empty(), "{what}");
        log.read_branch().unwrap();
        log.items().unwrap();
        assert_eq!(
            fs::read(dir.log()).unwrap(),
            &bytes[..cut],
            "{what}: opening changed the file"
        );
        drop(log);

        // For writing: the tail is repaired, dangling calls answered.
        let (mut session, report) = world.resume("s_1");
        if let Some(tail) = &report.cut_tail {
            let aside = report
                .cut_saved_to
                .as_ref()
                .expect("the cut bytes are kept");
            assert_eq!(fs::read(aside).unwrap(), &bytes[start..cut], "{what}");
            assert_eq!(tail.offset as usize, start, "{what}");
            fs::remove_file(aside).unwrap();
        }
        let dangling = if survived && kind == Kind::Assistant {
            2
        } else {
            0
        };
        assert_eq!(report.repaired.len(), dangling, "{what}");
        let next = session.append(None, user("and then")).unwrap();
        drop(session);

        let (log, report) = Log::open(&dir).unwrap();
        assert!(
            report.cut_tail.is_none() && report.damaged_lines.is_empty(),
            "{what}: {report:?}"
        );
        let expected = if survived { whole } else { whole - 1 } + dangling + 1;
        assert_eq!(log.len(), expected, "{what}");
        assert_eq!(log.active(), Some(&next), "{what}");
        // The branch reaches the first line, through the last entry if it
        // survived and is part of the tree.
        let branch = log.branch();
        assert_eq!(
            branch.first().map(|id| id.as_str()),
            Some("e_0000"),
            "{what}"
        );
        assert_eq!(
            branch.len(),
            expected - usize::from(survived && is_record),
            "{what}"
        );
    }
}

#[test]
fn a_log_cut_at_every_byte_of_a_last_prompt_opens_cleanly() {
    // Characters of two, three and four bytes, so that cuts fall inside a
    // character, and escapes.
    cut_at_every_byte("cut_prompt", user("é ✓ 🦀 \"quoted\"\nsecond line"));
}

#[test]
fn a_log_cut_at_every_byte_of_a_last_response_opens_cleanly() {
    cut_at_every_byte(
        "cut_response",
        assistant("i_5", "Two more: ünïcödé.", &["c_2", "c_3"]),
    );
}

#[test]
fn a_log_cut_at_every_byte_of_a_last_record_opens_cleanly() {
    cut_at_every_byte(
        "cut_record",
        Body::Meta(Meta {
            title: Some("A title".into()),
            ..Meta::default()
        }),
    );
}

#[test]
fn opening_read_only_never_changes_a_line_being_written() {
    let world = World::new("read_only_half_line");
    let (bytes, _) = session_ending_with(&world, user("done"));
    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    let half = br#"{"v":1,"id":"e_0006","parent":"e_0005","ts":"2026-11-12T10:00:00Z","kind":"user","item":{"id":"i_9","parts":[{"kind":"text","te"#;
    for partial in [&half[..], &half[..1], b"\n"] {
        let mut content = bytes.clone();
        content.extend_from_slice(partial);
        fs::write(dir.log(), &content).unwrap();
        let before = fs::metadata(dir.log()).unwrap();
        let (log, report) = Log::open(&dir).unwrap();
        assert_eq!(log.len(), 5);
        assert_eq!(report.cut_tail.is_some(), partial != b"\n");
        drop(log);
        assert_eq!(fs::read(dir.log()).unwrap(), content, "the file changed");
        assert_eq!(fs::metadata(dir.log()).unwrap().len(), before.len());
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|entry| { !entry.file_name().to_string_lossy().starts_with("log.cut") })
        );
    }
}

#[test]
fn a_reader_sees_a_session_while_its_writer_holds_it() {
    let world = World::new("reader_and_writer");
    let mut session = world.create("s_1");
    session.append(None, user("hello")).unwrap();
    let (log, report) = Log::open(session.dir()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(log.len(), 2);
    // A second writer is refused while the first holds the lock.
    let refused = Session::resume(&world.project, &SessionId::new("s_1"), world.options());
    assert!(
        matches!(refused, Err(SessionError::Busy { .. })),
        "{refused:?}"
    );
    drop(session);
    world.resume("s_1");
}

#[test]
fn damage_unknown_kinds_and_unknown_fields_are_skipped_and_reported() {
    let world = World::new("damage_and_unknown");
    let mut session = world.create("s_1");
    let first = session.append(None, user("hello")).unwrap();
    drop(session);
    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    let mut content = fs::read(dir.log()).unwrap();
    // A line a newer build wrote: a kind this build does not know, as a
    // step of the conversation, with fields of its own; then garbage; then
    // a known kind with a field this build does not know.
    let unknown = format!(
        r#"{{"v":1,"id":"e_0002","parent":"{first}","ts":"2026-11-12T10:00:01Z","kind":"hologram","beam":{{"power":9}}}}"#
    );
    content.extend_from_slice(unknown.as_bytes());
    content.extend_from_slice(b"\n{\"this is\": not json\n");
    content.extend_from_slice(
        br#"{"v":1,"id":"e_0003","parent":null,"ts":"2026-11-12T10:00:02Z","kind":"meta","title":"T","sparkle":true}"#,
    );
    content.push(b'\n');
    fs::write(dir.log(), &content).unwrap();

    let (log, report) = Log::open(&dir).unwrap();
    assert_eq!(report.damaged_lines, vec![4]);
    assert_eq!(report.unknown_kinds.get("hologram"), Some(&1));
    assert_eq!(log.active().map(|id| id.as_str()), Some("e_0002"));
    assert_eq!(
        log.items().unwrap().len(),
        1,
        "the unknown entry is not an item"
    );
    assert_eq!(log.title().unwrap().as_deref(), Some("T"));
    let unknown_entry = log.read(&"e_0002".into()).unwrap();
    assert!(
        matches!(&unknown_entry.body, Body::Unknown { kind, fields } if kind == "hologram" && fields["beam"] == json!({"power": 9}))
    );
    drop(log);

    // A resumed session goes on after it, and a fork copies it as it was.
    let (mut session, _) = world.resume("s_1");
    let next = session.append(None, user("more")).unwrap();
    assert_eq!(
        session.parent_of(&next).map(|id| id.as_str()),
        Some("e_0002")
    );
    assert_eq!(next.as_str(), "e_0004");
    let (fork, _) = session
        .fork(None, SessionId::new("s_2"), world.options())
        .unwrap();
    let forked = fs::read_to_string(fork.dir().log()).unwrap();
    assert!(forked.contains(&unknown), "{forked}");
}

#[test]
fn a_session_from_a_newer_format_opens_read_only() {
    let world = World::new("newer_format");
    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(
        dir.log(),
        concat!(
            r#"{"v":2,"id":"e_0000","parent":null,"ts":"2026-11-12T10:00:00Z","kind":"session","session":"s_1","workspace":"/w","harness":"9.0.0","warp":1}"#,
            "\n",
            r#"{"v":2,"id":"e_0001","parent":"e_0000","ts":"2026-11-12T10:00:01Z","kind":"user","item":{"id":"i_1","parts":[]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let (log, report) = Log::open(&dir).unwrap();
    assert!(report.read_only.is_some());
    assert_eq!(report.newer_entries, 2);
    assert_eq!(log.header().harness, "9.0.0");
    assert!(log.writable().is_err());
    assert!(matches!(
        log.fork(None, SessionId::new("s_2"), world.options()),
        Err(SessionError::ReadOnly { .. })
    ));
    assert!(matches!(
        Session::resume(&world.project, &SessionId::new("s_1"), world.options()),
        Err(SessionError::ReadOnly { .. })
    ));
}

#[test]
fn a_file_that_is_not_a_session_log_is_refused() {
    let world = World::new("not_a_log");
    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    fs::create_dir_all(dir.path()).unwrap();
    for content in [&b""[..], b"hello\n", br#"{"v":1,"id":"e_0000","parent":null,"ts":"t","kind":"session","session":"s_other","workspace":"/w","harness":"0"}"#] {
        fs::write(dir.log(), content).unwrap();
        assert!(matches!(Log::open(&dir), Err(SessionError::NotALog { .. })));
    }
}

#[test]
fn an_extension_record_round_trips_through_the_file() {
    let world = World::new("ext_record");
    let mut session = world.create("s_1");
    session.append(None, user("hi")).unwrap();
    let id = session
        .append(
            None,
            Body::Ext(ExtRecord {
                ext: "notes".into(),
                data: json!({"pinned": [1, 2]}),
            }),
        )
        .unwrap();
    assert!(session.is_record(&id));
    let entry = session.read(&id).unwrap();
    assert!(matches!(entry.body, Body::Ext(ExtRecord { ext, .. }) if ext == "notes"));
}

/// A line longer than the reader's limit would be accepted, and then
/// skipped as damage when the session is read again, with the branch after
/// it. So it is refused, and nothing is written.
#[test]
fn an_entry_over_the_line_limit_is_refused_and_nothing_is_lost() {
    let world = World::new("line_limit");
    let mut session = world.create("s_1");
    let first = session.append(None, user("first")).unwrap();
    let log_path = session.dir().log();
    let before = fs::read(&log_path).unwrap();
    // Each control character is six bytes as JSON (`\u0001`): a line over
    // 64 MiB from a quarter of the text, which keeps the test quick.
    let big = "\u{1}".repeat(11 * 1024 * 1024 + 1024);
    let refused = session.append(None, user(&big));
    drop(big);
    assert!(
        matches!(refused, Err(SessionError::EntryTooLarge { bytes, limit }) if bytes > limit),
        "{refused:?}"
    );
    assert_eq!(fs::read(&log_path).unwrap(), before, "nothing was written");
    assert_eq!(session.active(), Some(&first));
    let after = session.append(None, user("after!")).unwrap();
    assert_eq!(session.branch().len(), 3);
    drop(session);

    let dir = world.project.session(&SessionId::new("s_1")).unwrap();
    let (log, report) = Log::open(&dir).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(log.active(), Some(&after));
    assert_eq!(log.branch(), [EntryId::header(), first, after]);
}
