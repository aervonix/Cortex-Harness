//! Measuring the session store against its budgets
//! (`docs/performance-budgets.md`, "Data paths"). Not checks: the
//! performance tier compares the numbers. Run them in a release build:
//!
//! ```text
//! cargo test --release -p cortex-session --test it perf_ -- --ignored --nocapture
//! ```
//!
//! `CORTEX_TEST_TMPDIR` puts the files on another disk than the build's,
//! which matters for the flush.

use std::time::{Duration, Instant};

use cortex_protocol::ids::{SessionId, TurnId};
use cortex_session::entry::{Body, Checkpoint, FileChange, FileVersion};
use cortex_session::{Durability, Log, Session};

use crate::{World, assistant, tool_result, user};

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort_unstable();
    times[times.len() / 2]
}

fn percentile(mut times: Vec<Duration>, share: f64) -> Duration {
    times.sort_unstable();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an index into a short list"
    )]
    let index = ((times.len() - 1) as f64 * share).round() as usize;
    times[index]
}

/// A session of `entries` entries shaped like real work: prompts, answers
/// with a paragraph and a tool call, results of a kilobyte or two, and a
/// checkpoint now and then.
fn long_session(world: &World, entries: usize) -> u64 {
    let mut session = Session::create(
        &world.project,
        SessionId::new("s_long"),
        &world.root,
        world.options_with(Durability::Never),
    )
    .unwrap();
    let paragraph = "The function reads the configuration, checks each key against the schema, \
                     and reports the first error with its line. "
        .repeat(6);
    let output = "    let value = table.get(key).ok_or(Error::Missing)?;\n".repeat(30);
    let mut n = 0usize;
    let mut turn = 0usize;
    while session.len() < entries {
        turn += 1;
        let id = TurnId::new(format!("t_{turn}"));
        session
            .append(
                Some(&id),
                user("Look at the loader and fix the error message."),
            )
            .unwrap();
        for _ in 0..4 {
            n += 1;
            let call = format!("c_{n}");
            session
                .append(
                    Some(&id),
                    assistant(&format!("i_a{n}"), &paragraph, &[&call]),
                )
                .unwrap();
            let mut result = tool_result(&format!("i_r{n}"), &call);
            if let Body::ToolResult(item) = &mut result {
                item.content = vec![cortex_protocol::message::Part::text(output.clone())];
            }
            session.append(Some(&id), result).unwrap();
        }
        let hash = cortex_base::ContentHash::of(turn.to_string().as_bytes());
        session
            .append(
                Some(&id),
                Body::Checkpoint(Checkpoint {
                    files: vec![FileChange {
                        path: "src/loader.rs".into(),
                        before: Some(FileVersion {
                            hash,
                            bytes: 4000,
                            kept: true,
                        }),
                        after: Some(FileVersion {
                            hash,
                            bytes: 4100,
                            kept: true,
                        }),
                    }],
                }),
            )
            .unwrap();
    }
    session.end_turn().unwrap();
    session.bytes()
}

#[test]
#[ignore = "a measurement; run it in a release build"]
fn perf_open_a_session_of_ten_thousand_entries() {
    let world = World::new("perf_open");
    let bytes = long_session(&world, 10_000);
    let dir = world.project.session(&SessionId::new("s_long")).unwrap();
    let mut open = Vec::new();
    let mut resume = Vec::new();
    let mut branch = Vec::new();
    for _ in 0..15 {
        let started = Instant::now();
        let (log, _) = Log::open(&dir).unwrap();
        open.push(started.elapsed());
        let started = Instant::now();
        let items = log.items().unwrap();
        branch.push(started.elapsed());
        assert!(items.len() >= 8990, "{}", items.len());
        drop(log);
        let started = Instant::now();
        let (session, _) =
            Session::resume(&world.project, &SessionId::new("s_long"), world.options()).unwrap();
        resume.push(started.elapsed());
        drop(session);
    }
    println!(
        "open 10,000 entries ({:.1} MB): open {:?}, resume {:?}, then read the active branch's items {:?} (medians of 15)",
        bytes as f64 / 1e6,
        median(open),
        median(resume),
        median(branch),
    );
}

#[test]
#[ignore = "a measurement; run it in a release build"]
fn perf_list_a_thousand_sessions() {
    let world = World::new("perf_list");
    for number in 0..1000 {
        let mut session = Session::create(
            &world.project,
            SessionId::new(format!("s_{number:04}")),
            &world.root,
            world.options_with(Durability::Never),
        )
        .unwrap();
        session
            .append(None, user("Look at the loader and fix the error message."))
            .unwrap();
        world.clock.advance(Duration::from_millis(3));
    }
    let index = world.project.index();
    assert_eq!(index.list().unwrap().len(), 1000);
    let mut times = Vec::new();
    for _ in 0..30 {
        let started = Instant::now();
        let listed = index.list().unwrap();
        times.push(started.elapsed());
        assert_eq!(listed.len(), 1000);
    }
    println!(
        "list 1,000 sessions: median {:?}, slowest {:?}",
        median(times.clone()),
        times.iter().max().unwrap()
    );
}

#[test]
#[ignore = "a measurement; run it on the disk sessions live on"]
fn perf_append_one_entry_with_its_flush() {
    let world = World::new("perf_append");
    for durability in [Durability::Item, Durability::Never] {
        let id = format!("s_{}", durability.as_str());
        let mut session = Session::create(
            &world.project,
            SessionId::new(id),
            &world.root,
            world.options_with(durability),
        )
        .unwrap();
        session.append(None, user("start")).unwrap();
        let mut times = Vec::new();
        for n in 0..200 {
            let body = assistant(
                &format!("i_{n}"),
                "A short answer with a little text in it.",
                &[],
            );
            let started = Instant::now();
            session.append(None, body).unwrap();
            times.push(started.elapsed());
        }
        println!(
            "append one entry, durability `{}`: median {:?}, 95th percentile {:?}",
            durability.as_str(),
            median(times.clone()),
            percentile(times, 0.95)
        );
    }
}
