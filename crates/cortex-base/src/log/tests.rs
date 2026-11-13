//! Tests of the subscriber as a whole: events and spans in, lines out.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "tests read the log file they made"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tracing::metadata::LevelFilter;

use super::*;
use crate::clock::FakeClock;
use crate::secret::Secret;

const KEY: &str = "cortex-log-test-key-91c4";

/// Somewhere to write that the test can read back.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Buffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Buffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Rig {
    dir: PathBuf,
    clock: FakeClock,
    stderr: Buffer,
    options: Option<LogOptions>,
}

impl Rig {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("cortex-base-log")
            .join(format!("{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let clock = FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap();
        let stderr = Buffer::default();
        let options = LogOptions {
            filter: Filter::default(),
            file: Some(FileOptions::in_dir(&dir)),
            stderr: None,
            secrets: Arc::new(SecretFilter::new().with_value(&Secret::new(KEY))),
            clock: Arc::new(clock.clone()),
        };
        Self {
            dir,
            clock,
            stderr,
            options: Some(options),
        }
    }

    fn with_stderr(mut self, level: LevelFilter) -> Self {
        if let Some(options) = &mut self.options {
            options.stderr = Some((level, Box::new(self.stderr.clone())));
        }
        self
    }

    fn filter(mut self, text: &str) -> Self {
        if let Some(options) = &mut self.options {
            options.filter = Filter::parse(text).unwrap();
        }
        self
    }

    /// Runs `work` with the subscriber installed for this thread, and
    /// returns the lines of the log file.
    fn run(&mut self, work: impl FnOnce(&LogHandle)) -> Vec<Value> {
        let (subscriber, handle) = start(self.options.take().unwrap()).unwrap();
        tracing::subscriber::with_default(subscriber, || work(&handle));
        handle.flush();
        let text = std::fs::read_to_string(self.dir.join(FILE_NAME)).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn an_event_is_one_json_line_with_its_fields_and_spans() {
    let mut rig = Rig::new("event");
    let lines = rig.run(|_| {
        let turn = tracing::info_span!("turn", turn = "t_1");
        let _in_turn = turn.enter();
        let request = tracing::info_span!("request", attempt = 2u64);
        let _in_request = request.enter();
        tracing::warn!(status = 529, retry = true, "the provider is overloaded");
    });
    let line = &lines[0];
    assert_eq!(line["ts"], "2026-11-13T10:00:00Z");
    assert_eq!(line["level"], "warn");
    assert_eq!(line["target"], "cortex_base::log::tests");
    assert_eq!(line["msg"], "the provider is overloaded");
    assert_eq!(line["fields"]["status"], 529);
    assert_eq!(line["fields"]["retry"], true);
    // Outermost first, each with its own fields.
    assert_eq!(line["spans"][0]["name"], "turn");
    assert_eq!(line["spans"][0]["turn"], "t_1");
    assert_eq!(line["spans"][1]["name"], "request");
    assert_eq!(line["spans"][1]["attempt"], 2);
    // Each span says when it closed, innermost first.
    assert_eq!(lines[1]["msg"], "span closed");
    assert_eq!(lines[1]["fields"]["span"], "request");
    assert_eq!(lines[2]["fields"]["span"], "turn");
    assert_eq!(lines.len(), 3);
}

#[test]
fn the_filter_decides_by_level_and_by_target_prefix() {
    let mut rig = Rig::new("filter").filter("warn,cortex_base::log=debug,noisy=off");
    let lines = rig.run(|_| {
        tracing::debug!("kept: this module is at debug");
        tracing::trace!("dropped: more detail than debug");
        tracing::info!(target: "cortex_llm::client", "dropped: others are at warn");
        tracing::warn!(target: "cortex_llm::client", "kept: a warning");
        tracing::error!(target: "noisy::thing", "dropped: turned off");
    });
    let messages: Vec<&str> = lines
        .iter()
        .map(|line| line["msg"].as_str().unwrap())
        .collect();
    assert_eq!(
        messages,
        ["kept: this module is at debug", "kept: a warning"]
    );
}

#[test]
fn the_filter_can_be_changed_while_the_process_runs() {
    let mut rig = Rig::new("refilter");
    let lines = rig.run(|handle| {
        tracing::debug!("before");
        handle.set_filter(Filter::parse("debug").unwrap());
        tracing::debug!("after");
        handle.set_filter(Filter::parse("off").unwrap());
        tracing::error!("silenced");
    });
    let messages: Vec<&str> = lines
        .iter()
        .map(|line| line["msg"].as_str().unwrap())
        .collect();
    assert_eq!(messages, ["after"]);
}

#[test]
fn the_stderr_sink_is_off_unless_it_is_given_a_level() {
    let mut rig = Rig::new("stderr-off");
    let stderr = rig.stderr.clone();
    let lines = rig.run(|handle| {
        tracing::error!("only in the file");
        // With no sink there is no level to change.
        handle.set_stderr_level(LevelFilter::TRACE);
        tracing::error!("still only in the file");
    });
    assert_eq!(lines.len(), 2);
    assert_eq!(stderr.text(), "");
}

#[test]
fn the_stderr_sink_takes_its_own_level_and_writes_for_a_person() {
    // The file is off for everything; standard error takes warnings.
    let mut rig = Rig::new("stderr")
        .filter("off")
        .with_stderr(LevelFilter::WARN);
    let stderr = rig.stderr.clone();
    let lines = rig.run(|handle| {
        let turn = tracing::info_span!("turn", turn = "t_1");
        let _in_turn = turn.enter();
        tracing::info!("not a warning");
        tracing::warn!(attempt = 2, reason = "overloaded", "retrying");
        handle.set_stderr_level(LevelFilter::OFF);
        tracing::error!("quiet now");
        handle.set_stderr_level(LevelFilter::ERROR);
        tracing::error!("loud again");
    });
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(
        stderr.text(),
        "warn: retrying attempt=2 reason=overloaded (cortex_base::log::tests)\n\
         error: loud again (cortex_base::log::tests)\n"
    );
}

#[test]
fn a_span_the_file_does_not_take_is_not_named_on_stderr() {
    // The span is at `info`, which nothing wants here, so it is never
    // opened: the line stands alone.
    let mut rig = Rig::new("stderr-span")
        .filter("off")
        .with_stderr(LevelFilter::WARN);
    let stderr = rig.stderr.clone();
    rig.run(|_| {
        let turn = tracing::warn_span!("turn");
        let _in_turn = turn.enter();
        tracing::warn!("inside");
    });
    assert_eq!(
        stderr.text(),
        "warn: inside (cortex_base::log::tests > turn)\n\
         warn: span closed busy_ms=0.0 elapsed_ms=0.0 span=turn (cortex_base::log::tests)\n"
    );
}

#[test]
fn a_secret_is_masked_in_a_message_a_field_and_a_span() {
    let mut rig = Rig::new("secret").with_stderr(LevelFilter::INFO);
    let stderr = rig.stderr.clone();
    let lines = rig.run(|_| {
        let span = tracing::info_span!("request", header = %format!("x-api-key: {KEY}"));
        let _entered = span.enter();
        tracing::info!(body = %format!("{{\"key\":\"{KEY}\"}}"), "sent with {KEY}");
        tracing::info!(token = ?Some("ghp_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8"), "a token");
    });
    let file = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for output in [file, stderr.text()] {
        assert!(!output.contains(KEY), "{output}");
        assert!(!output.contains("ghp_"), "{output}");
        assert!(output.contains("[redacted]"), "{output}");
    }
    assert_eq!(lines[0]["msg"], "sent with [redacted]");
    assert_eq!(lines[0]["spans"][0]["header"], "x-api-key: [redacted]");
}

#[test]
fn a_long_value_is_cut_and_says_by_how_much() {
    let mut rig = Rig::new("long");
    let long = "é".repeat(MAX_VALUE);
    let lines = rig.run(|_| tracing::info!(body = %long, "a body"));
    let body = lines[0]["fields"]["body"].as_str().unwrap();
    assert!(body.len() < MAX_VALUE + 40, "{}", body.len());
    assert!(
        body.ends_with(&format!("… [{} more bytes]", MAX_VALUE)),
        "{body}"
    );
    assert!(body.starts_with("ééé"));
}

#[test]
fn span_timings_say_when_a_span_opened_and_closed_and_how_long_it_was_busy() {
    let mut rig = Rig::new("timings");
    let clock = rig.clock.clone();
    let mut timings = None;
    rig.run(|handle| {
        timings = Some(handle.timings());
        clock.advance(Duration::from_millis(5));
        let turn = tracing::info_span!("turn", turn = "t_7");
        {
            let _entered = turn.enter();
            clock.advance(Duration::from_millis(10));
            // Prompt to request sent: the request span opens inside the
            // turn, 10 ms after it.
            let request = tracing::info_span!("request");
            {
                let _in_request = request.enter();
                clock.advance(Duration::from_millis(3));
            }
            // Waiting for the response: open, and not entered.
            clock.advance(Duration::from_millis(100));
            {
                let _in_request = request.enter();
                clock.advance(Duration::from_millis(2));
            }
        }
        clock.advance(Duration::from_millis(50));
        // A clone keeps the span open.
        let kept = turn.clone();
        drop(turn);
        clock.advance(Duration::from_millis(1));
        drop(kept);
    });
    let timings = timings.unwrap();
    let all = timings.all();
    assert_eq!(all.len(), 2);
    let request = &timings.named("request")[0];
    let turn = &timings.named("turn")[0];
    assert_eq!(turn.opened, Duration::from_millis(5));
    assert_eq!(turn.field("turn"), Some("t_7"));
    assert_eq!(turn.parent, None);
    assert_eq!(request.parent, Some(turn.id));
    // Prompt to request sent.
    assert_eq!(request.opened - turn.opened, Duration::from_millis(10));
    assert_eq!(request.elapsed(), Duration::from_millis(105));
    assert_eq!(request.busy, Duration::from_millis(5));
    assert_eq!(turn.busy, Duration::from_millis(115));
    assert_eq!(turn.elapsed(), Duration::from_millis(166));
    // Taking them leaves none.
    assert_eq!(timings.take().len(), 2);
    assert!(timings.all().is_empty());
}

#[test]
fn spans_are_timed_with_no_file_to_write() {
    let clock = FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap();
    let (subscriber, handle) = start(LogOptions::new(Arc::new(clock.clone()))).unwrap();
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("tool", tool = "read");
        let _entered = span.enter();
        clock.advance(Duration::from_millis(7));
        tracing::info!("nowhere to go, and no harm");
    });
    let timings = handle.timings().all();
    assert_eq!(timings.len(), 1);
    assert_eq!(timings[0].busy, Duration::from_millis(7));
    assert_eq!(handle.dropped(), 0);
}

#[test]
fn only_so_many_closed_spans_are_kept() {
    let clock = FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap();
    let (subscriber, handle) = start(LogOptions::new(Arc::new(clock))).unwrap();
    tracing::subscriber::with_default(subscriber, || {
        for n in 0..TIMINGS_KEPT + 10 {
            let _span = tracing::info_span!("tool", n = n as u64);
        }
    });
    let timings = handle.timings().all();
    assert_eq!(timings.len(), TIMINGS_KEPT);
    assert_eq!(timings[0].field("n"), Some("10"));
}

#[test]
fn an_event_can_name_its_span_and_the_current_span_is_known() {
    let mut rig = Rig::new("parent");
    let lines = rig.run(|_| {
        let outer = tracing::info_span!("session");
        let other = tracing::info_span!(parent: None, "detached");
        let _entered = outer.enter();
        assert_eq!(
            tracing::Span::current().metadata().map(Metadata::name),
            Some("session")
        );
        tracing::info!(parent: &other, "in the other span");
        tracing::info!(parent: None, "in none");
    });
    assert_eq!(lines[0]["spans"][0]["name"], "detached");
    assert_eq!(lines[0]["spans"].as_array().unwrap().len(), 1);
    assert!(lines[1].get("spans").is_none(), "{}", lines[1]);
}
