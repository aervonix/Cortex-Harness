//! Where a test's time went (`docs/testing-and-ci.md`, "Where a test's time
//! goes").
//!
//! nextest reports a test's whole time. With `CORTEX_TESTKIT_PROFILE=1` a
//! test that uses the test kit prints, when it ends, what that time was
//! made of:
//!
//! - **process start:** how long the process ran before the test kit was
//!   first used: the system starting it, the test harness, and anything the
//!   test did before (Linux only, where the system says when a process
//!   started);
//! - **phases:** the set-up and tear-down a rig marks, and any phase a test
//!   names with [`phase`];
//! - **waits:** every wait of the test kit's own (a [`Gate`](crate::Gate),
//!   the scripted server's steps, a [`Roll`](crate::children::Roll), a
//!   child's [`until`](crate::children::until)), with the thread that
//!   waited: the same waits that the timeout report names
//!   ([`watch`](crate::watch)).
//!
//! The report is written to standard error when the test's
//! [`Report`] is dropped. A [`Scratch`](crate::Scratch) holds one, so a
//! test with a directory of its own, and every rig built on one, is
//! profiled with no change. nextest shows a passing test's output with
//! `--no-capture`:
//!
//! ```text
//! CORTEX_TESTKIT_PROFILE=1 cargo xtask test -p cortex-agent -E 'test(=x)' --no-capture
//! ```
//!
//! **It costs nothing when it is off.** The variable is read once, and each
//! mark asks one flag after that: no clock is read, nothing is allocated
//! and no lock is taken.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// The variable that turns the profile on.
pub const VARIABLE: &str = "CORTEX_TESTKIT_PROFILE";

/// What kind of time a span is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A phase: set-up, tear-down, or one the test named.
    Phase,
    /// A wait of the test kit's own.
    Wait,
}

/// A stretch of time on one thread.
#[derive(Debug, Clone)]
struct Span {
    kind: Kind,
    what: String,
    thread: String,
    start: Instant,
    end: Instant,
}

/// What this process has recorded.
struct Recorder {
    /// When profiling was first asked for, and how old the process was
    /// then, where the system says.
    origin: Instant,
    age: Option<Duration>,
    spans: Mutex<Vec<Span>>,
    tear_down: Mutex<Option<Instant>>,
}

/// 0: not read yet; 1: off; 2: on.
static STATE: AtomicU8 = AtomicU8::new(0);
static RECORDER: OnceLock<Recorder> = OnceLock::new();

/// Whether the profile is on in this process. The variable is read the
/// first time; after that this is one load of a flag.
#[must_use]
pub fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var_os(VARIABLE).is_some_and(|value| value == "1");
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// The recorder, when the profile is on.
fn recorder() -> Option<&'static Recorder> {
    if !enabled() {
        return None;
    }
    Some(RECORDER.get_or_init(|| Recorder {
        origin: Instant::now(),
        age: crate::watch::process_age(),
        spans: Mutex::new(Vec::new()),
        tear_down: Mutex::new(None),
    }))
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("an unnamed thread")
        .to_owned()
}

impl Recorder {
    fn record(&self, kind: Kind, what: String, start: Instant) {
        let span = Span {
            kind,
            what,
            thread: thread_name(),
            start,
            end: Instant::now(),
        };
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(span);
    }
}

/// A span that is recorded when it is dropped. When the profile is off it
/// holds nothing.
#[derive(Debug)]
#[must_use = "the span ends when this is dropped"]
pub struct Mark {
    open: Option<(Kind, String, Instant)>,
}

impl Mark {
    fn start(kind: Kind, what: impl FnOnce() -> String) -> Self {
        Self {
            open: recorder().map(|_| (kind, what(), Instant::now())),
        }
    }

    /// Ends the span here and starts another on the same thread: a wait
    /// that moves to its next step.
    pub fn next(&mut self, what: impl FnOnce() -> String) {
        if let Some((kind, _, _)) = &self.open {
            let kind = *kind;
            self.finish();
            self.open = Some((kind, what(), Instant::now()));
        }
    }

    fn finish(&mut self) {
        if let (Some((kind, what, start)), Some(recorder)) = (self.open.take(), recorder()) {
            recorder.record(kind, what, start);
        }
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        self.finish();
    }
}

/// A phase of the test, named, until the guard is dropped.
pub fn phase(name: &str) -> Mark {
    Mark::start(Kind::Phase, || name.to_owned())
}

/// A wait of the test kit's, described when the profile is on.
pub(crate) fn wait(what: impl FnOnce() -> String) -> Mark {
    Mark::start(Kind::Wait, what)
}

/// Marks where the test's tear-down begins: a rig calls it first thing in
/// its `Drop`. The report counts from there to its own end as tear-down.
pub fn tear_down() {
    if let Some(recorder) = recorder() {
        *recorder
            .tear_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }
}

/// The report of one test, written to standard error when this is dropped.
#[derive(Debug)]
pub struct Report {
    test: String,
    started: Option<Instant>,
    /// The thread that made it, the test's own: its spans are not named
    /// by thread.
    thread: String,
}

/// Starts the report of the test named. A [`Scratch`](crate::Scratch)
/// starts one.
#[must_use]
pub fn report(test: &str) -> Report {
    let started = recorder().map(|_| Instant::now());
    Report {
        test: test.to_owned(),
        thread: started.map(|_| thread_name()).unwrap_or_default(),
        started,
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        let (Some(started), Some(recorder)) = (self.started, recorder()) else {
            return;
        };
        let end = Instant::now();
        let spans = recorder
            .spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let tear_down = *recorder
            .tear_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let text = render(&Rendering {
            test: &self.test,
            thread: &self.thread,
            origin: recorder.origin,
            age: recorder.age,
            started,
            end,
            tear_down: tear_down.filter(|at| *at >= started),
            spans: &spans,
        });
        let mut error = std::io::stderr().lock();
        let _ = std::io::Write::write_all(&mut error, text.as_bytes());
        let _ = std::io::Write::flush(&mut error);
    }
}

/// What a report is made from.
struct Rendering<'a> {
    test: &'a str,
    thread: &'a str,
    origin: Instant,
    age: Option<Duration>,
    started: Instant,
    end: Instant,
    tear_down: Option<Instant>,
    spans: &'a [Span],
}

fn ms(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

/// The report as text: a line for each span that overlaps the test's time,
/// in the order they began, at its distance from the process's start (or
/// from the first use of the test kit, where that is not known).
fn render(report: &Rendering<'_>) -> String {
    let zero = report.age.unwrap_or_default();
    let at = |instant: Instant| zero + instant.saturating_duration_since(report.origin);
    let total = at(report.end);
    let mut text = String::new();
    let _ = writeln!(
        text,
        "\ntime profile of `{}` ({VARIABLE}): {} {}",
        report.test,
        ms(total),
        if report.age.is_some() {
            "from the process's start"
        } else {
            "from the test kit's first use (this system does not say when a process started)"
        }
    );
    let _ = writeln!(text, "{:>11} {:>11}  what", "at", "took");
    if let Some(age) = report.age {
        let _ = writeln!(
            text,
            "{:>11} {:>11}  process start, before the test kit was first used",
            ms(Duration::ZERO),
            ms(age)
        );
    }
    let mut spans: Vec<&Span> = report
        .spans
        .iter()
        .filter(|span| span.end >= report.started && span.start <= report.end)
        .collect();
    spans.sort_by_key(|span| span.start);
    let mut waits = Duration::ZERO;
    // The stretches the phases and the tear-down cover, to tell what is in
    // none of them.
    let mut covered: Vec<(Instant, Instant)> = Vec::new();
    for span in &spans {
        let took = span.end.saturating_duration_since(span.start);
        let (label, indent) = match span.kind {
            Kind::Phase => {
                covered.push((span.start, span.end));
                ("phase", "")
            }
            Kind::Wait => {
                waits += took;
                ("wait", "  ")
            }
        };
        let thread = if span.thread == report.thread {
            String::new()
        } else {
            format!(" [{}]", span.thread)
        };
        let _ = writeln!(
            text,
            "{:>11} {:>11}  {indent}{label}: {}{thread}",
            ms(at(span.start)),
            ms(took),
            span.what,
        );
    }
    if let Some(begun) = report.tear_down {
        covered.push((begun, report.end));
        let _ = writeln!(
            text,
            "{:>11} {:>11}  tear-down",
            ms(at(begun)),
            ms(report.end.saturating_duration_since(begun))
        );
    }
    let in_phases = union(&mut covered, report.origin, report.end);
    let measured = report.end.saturating_duration_since(report.origin);
    let _ = writeln!(
        text,
        "{:>11} {:>11}  in phases and tear-down; {} in the test kit's waits, on any thread; {} in \
         none of them (the test's own work)",
        "",
        ms(in_phases),
        ms(waits),
        ms(measured.saturating_sub(in_phases))
    );
    text
}

/// How much of `from..to` the stretches cover, each counted once.
fn union(stretches: &mut [(Instant, Instant)], from: Instant, to: Instant) -> Duration {
    stretches.sort_by_key(|stretch| stretch.0);
    let mut total = Duration::ZERO;
    let mut reached = from;
    for &(start, end) in stretches.iter() {
        let (start, end) = (start.max(reached), end.min(to));
        if end > start {
            total += end - start;
            reached = end;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Kind, Mark, Rendering, Span, enabled, render};

    #[test]
    fn off_it_reads_no_clock_and_keeps_nothing() {
        if std::env::var_os(super::VARIABLE).is_some() {
            return;
        }
        assert!(!enabled());
        // No span is opened: no name is made, no clock read.
        let mut made = 0;
        let mut mark = Mark::start(Kind::Phase, || {
            made += 1;
            "set-up".to_owned()
        });
        assert!(mark.open.is_none());
        mark.next(|| {
            made += 1;
            "next".to_owned()
        });
        drop(mark);
        assert_eq!(made, 0, "a span was described while the profile was off");
        assert!(super::report("x").started.is_none());
        assert!(super::RECORDER.get().is_none(), "the recorder was made");
    }

    #[test]
    fn a_report_lists_the_phases_and_waits_in_order_with_the_start() {
        let origin = Instant::now();
        let later = |ms: u64| origin + Duration::from_millis(ms);
        let span = |kind, what: &str, start, end| Span {
            kind,
            what: what.to_owned(),
            thread: "main".to_owned(),
            start: later(start),
            end: later(end),
        };
        let spans = [
            Span {
                thread: "scripted-server-1".to_owned(),
                ..span(Kind::Wait, "waits for the gate made at a.rs:1:2", 30, 70)
            },
            span(Kind::Phase, "set-up", 0, 20),
            span(Kind::Phase, "run the binary", 25, 90),
        ];
        let text = render(&Rendering {
            test: "agent::x",
            thread: "main",
            origin,
            age: Some(Duration::from_millis(12)),
            started: origin,
            end: later(100),
            tear_down: Some(later(90)),
            spans: &spans,
        });
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].contains("time profile of `agent::x`"), "{text}");
        assert!(
            lines[1].contains("112.0 ms from the process's start"),
            "{text}"
        );
        assert!(lines[3].ends_with("process start, before the test kit was first used"));
        assert!(lines[3].contains("12.0 ms"), "{text}");
        assert!(lines[4].contains("12.0 ms") && lines[4].ends_with("phase: set-up"));
        assert!(lines[5].contains("37.0 ms") && lines[5].contains("phase: run the binary"));
        assert!(lines[6].contains("42.0 ms") && lines[6].contains("  wait: waits for the gate"));
        assert!(lines[6].ends_with("a.rs:1:2 [scripted-server-1]"), "{text}");
        assert!(
            lines[7].ends_with("tear-down") && lines[7].contains("10.0 ms"),
            "{text}"
        );
        // Set-up 0..20, the binary 25..90 and tear-down 90..100: 95 ms,
        // and 5 ms in none of them.
        assert!(
            lines[8].contains("95.0 ms  in phases and tear-down"),
            "{text}"
        );
        assert!(
            lines[8].contains("40.0 ms in the test kit's waits"),
            "{text}"
        );
        assert!(lines[8].contains("5.0 ms in none of them"), "{text}");
    }
}
