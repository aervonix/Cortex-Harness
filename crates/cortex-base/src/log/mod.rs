//! Logging: a small subscriber of our own for the `tracing` facade
//! ([decision 0019](../../../docs/decisions/0019-logging.md),
//! `docs/architecture/02-runtime.md`, "Logging and tracing").
//!
//! Every crate logs through `tracing`'s macros. This module is what
//! receives them:
//!
//! - **A filter** ([`Filter`]): a level, and levels of their own for
//!   targets under a prefix. It can be changed while the process runs.
//! - **The log file** ([`FileOptions`]): JSON lines, written by a thread of
//!   its own through a bounded queue, rotated by size, owner-only.
//! - **A stderr sink**, off unless it is given a level: one readable line
//!   per event, for headless and RPC modes. Never standard output.
//! - **Span timings** ([`Timings`]): when each span opened and closed and
//!   how long it was busy, kept for the performance probes
//!   (`docs/performance-budgets.md`).
//!
//! **Nothing here writes a secret it can recognise.** Every message and
//! every field passes the [`SecretFilter`] before it reaches a sink, and a
//! long value is cut. That is a second line of defence: the first is that
//! credentials are [`Secret`](crate::Secret) values, which print as
//! `[redacted]`, and that prompt and file content is logged at `debug`
//! only.
//!
//! **What it is not.** It is not a general subscriber: no layers, no
//! per-span filtering, no formatting options. And it does not bridge
//! dependencies that log through the `log` crate, which decision 0019
//! names: that needs a second facade as a dependency, and is left open.
//!
//! # Use
//!
//! ```text
//! let (subscriber, handle) = cortex_base::log::start(options)?;
//! tracing::subscriber::set_global_default(subscriber)?;
//! // … at the end of the process:
//! handle.flush();
//! ```

mod file;
mod filter;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::metadata::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata};
use tracing_core::span::Current;

pub use file::{DEFAULT_FILES, DEFAULT_MAX_BYTES, FILE_NAME, FileOptions};
pub use filter::{DEFAULT_LEVEL, Filter, FilterError, level_name};

use crate::clock::SharedClock;
use crate::redact::SecretFilter;
use file::FileSink;

/// The longest field value or message a line holds, in bytes. A longer one
/// is cut, and says by how much.
pub const MAX_VALUE: usize = 8 * 1024;

/// How many closed spans are kept for the probes. Older ones are dropped.
pub const TIMINGS_KEPT: usize = 1024;

/// What the subscriber is made from.
pub struct LogOptions {
    /// Which lines the log file takes.
    pub filter: Filter,
    /// The log file. `None` writes no file.
    pub file: Option<FileOptions>,
    /// The stderr sink: the level it takes, and where it writes. `None` is
    /// off, which is what the terminal UI runs with.
    pub stderr: Option<(LevelFilter, Box<dyn Write + Send>)>,
    /// Masks credentials in everything that is written.
    pub secrets: Arc<SecretFilter>,
    /// The time of each line, and the monotonic time of each span.
    pub clock: SharedClock,
}

impl std::fmt::Debug for LogOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogOptions")
            .field("filter", &self.filter)
            .field("file", &self.file)
            .field("stderr", &self.stderr.as_ref().map(|(level, _)| level))
            .finish_non_exhaustive()
    }
}

impl LogOptions {
    /// The default filter, no file and no stderr sink.
    #[must_use]
    pub fn new(clock: SharedClock) -> Self {
        Self {
            filter: Filter::default(),
            file: None,
            stderr: None,
            secrets: Arc::new(SecretFilter::new()),
            clock,
        }
    }
}

/// One closed span, for the performance probes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanTiming {
    /// The span's id while it was open. Ids are used again.
    pub id: u64,
    /// The span it was opened inside.
    pub parent: Option<u64>,
    /// Its name: `turn`, `request`, `tool`.
    pub name: &'static str,
    /// The module that opened it.
    pub target: &'static str,
    /// Its fields, as text.
    pub fields: Vec<(String, String)>,
    /// When it opened, on the clock's monotonic time.
    pub opened: Duration,
    /// When it closed.
    pub closed: Duration,
    /// How long it was entered: for a span around a future, the time spent
    /// polling it.
    pub busy: Duration,
}

impl SpanTiming {
    /// From its opening to its close.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.closed.saturating_sub(self.opened)
    }

    /// The value of one of its fields.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The spans that have closed, newest last. Clones read the same list.
#[derive(Debug, Clone, Default)]
pub struct Timings {
    closed: Arc<Mutex<VecDeque<SpanTiming>>>,
}

impl Timings {
    /// Every kept span, oldest first.
    #[must_use]
    pub fn all(&self) -> Vec<SpanTiming> {
        crate::sync::lock(&self.closed).iter().cloned().collect()
    }

    /// The kept spans with this name, oldest first.
    #[must_use]
    pub fn named(&self, name: &str) -> Vec<SpanTiming> {
        crate::sync::lock(&self.closed)
            .iter()
            .filter(|timing| timing.name == name)
            .cloned()
            .collect()
    }

    /// Takes every kept span, leaving none.
    #[must_use]
    pub fn take(&self) -> Vec<SpanTiming> {
        crate::sync::lock(&self.closed).drain(..).collect()
    }

    fn push(&self, timing: SpanTiming) {
        let mut closed = crate::sync::lock(&self.closed);
        if closed.len() == TIMINGS_KEPT {
            closed.pop_front();
        }
        closed.push_back(timing);
    }
}

/// What a span is while it is open.
struct OpenSpan {
    metadata: &'static Metadata<'static>,
    parent: Option<u64>,
    fields: Map<String, Value>,
    opened: Duration,
    busy: Duration,
    /// When it was last entered, and how many times it is entered now.
    entered: Option<(Duration, u32)>,
    /// How many handles there are to it.
    handles: usize,
}

struct Stderr {
    level: LevelFilter,
    out: Box<dyn Write + Send>,
}

struct Shared {
    filter: RwLock<Filter>,
    file: Option<FileSink>,
    stderr: Mutex<Option<Stderr>>,
    /// The stderr sink's level, readable without its lock: 0 is off, and
    /// 1 to 5 are `error` to `trace`.
    stderr_level: AtomicU64,
    /// Replaced whole when a key is added, so that a line being written
    /// never waits for more than the swap.
    secrets: RwLock<Arc<SecretFilter>>,
    clock: SharedClock,
    spans: Mutex<HashMap<u64, OpenSpan>>,
    next_id: AtomicU64,
    timings: Timings,
}

thread_local! {
    /// The spans this thread is inside, innermost last.
    static ENTERED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// The subscriber: give it to `tracing::subscriber::set_global_default`.
pub struct LogSubscriber {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for LogSubscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogSubscriber").finish_non_exhaustive()
    }
}

/// How the rest of the process reaches the subscriber once it is
/// installed: to change what is logged, to read span timings, and to wait
/// for the file at the end.
pub struct LogHandle {
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for LogHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogHandle").finish_non_exhaustive()
    }
}

/// Makes the subscriber, and starts the log-writer thread when there is a
/// file to write.
///
/// # Errors
///
/// When the log directory cannot be made or the file cannot be opened. A
/// caller that would rather run without a log file than not run calls
/// again with `file: None`.
pub fn start(options: LogOptions) -> io::Result<(LogSubscriber, LogHandle)> {
    let (file, writer) = match options.file {
        Some(file) => {
            let (sink, thread) = FileSink::start(file)?;
            (Some(sink), Some(thread))
        }
        None => (None, None),
    };
    let stderr_level = options
        .stderr
        .as_ref()
        .map_or(0, |(level, _)| level_number(*level));
    let shared = Arc::new(Shared {
        filter: RwLock::new(options.filter),
        file,
        stderr: Mutex::new(options.stderr.map(|(level, out)| Stderr { level, out })),
        stderr_level: AtomicU64::new(stderr_level),
        secrets: RwLock::new(options.secrets),
        clock: options.clock,
        spans: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        timings: Timings::default(),
    });
    Ok((
        LogSubscriber {
            shared: Arc::clone(&shared),
        },
        LogHandle { shared, writer },
    ))
}

fn level_number(level: LevelFilter) -> u64 {
    match level.into_level() {
        None => 0,
        Some(Level::ERROR) => 1,
        Some(Level::WARN) => 2,
        Some(Level::INFO) => 3,
        Some(Level::DEBUG) => 4,
        Some(Level::TRACE) => 5,
    }
}

impl LogHandle {
    /// Replaces the filter of the log file: what `--log-level`,
    /// `CORTEX_LOG` or a reloaded `log.level` asks for.
    pub fn set_filter(&self, filter: Filter) {
        *self
            .shared
            .filter
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = filter;
        // Call sites remember whether they were wanted.
        tracing::callsite::rebuild_interest_cache();
    }

    /// Changes the level of the stderr sink, when there is one. `OFF`
    /// silences it (`--quiet`).
    pub fn set_stderr_level(&self, level: LevelFilter) {
        if let Some(stderr) = crate::sync::lock(&self.shared.stderr).as_mut() {
            stderr.level = level;
            self.shared
                .stderr_level
                .store(level_number(level), Ordering::Relaxed);
        }
        tracing::callsite::rebuild_interest_cache();
    }

    /// Also masks `secret` by value in everything logged from now on. The
    /// credentials are found after logging starts (a session finds its
    /// provider's key when it is put together), so whoever finds one
    /// hands it over here.
    pub fn add_secret(&self, secret: &crate::Secret) {
        let mut secrets = self
            .shared
            .secrets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !secrets.knows(secret) {
            let mut wider = SecretFilter::clone(&secrets);
            wider.add_value(secret);
            *secrets = Arc::new(wider);
        }
    }

    /// The spans that have closed.
    #[must_use]
    pub fn timings(&self) -> Timings {
        self.shared.timings.clone()
    }

    /// Waits until every line logged so far is in the file. It blocks on
    /// the disk: call it at the end of the process, not on the core thread
    /// while a session runs.
    pub fn flush(&self) {
        if let Some(file) = &self.shared.file {
            file.flush();
        }
        if let Some(stderr) = crate::sync::lock(&self.shared.stderr).as_mut() {
            let _ = stderr.out.flush();
        }
    }

    /// How many lines were dropped because the writer could not keep up
    /// and have not been reported in the file yet.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.file.as_ref().map_or(0, FileSink::dropped)
    }
}

impl Drop for LogHandle {
    fn drop(&mut self) {
        self.flush();
        // The thread ends with its last sender, which an installed
        // subscriber keeps for the life of the process: it is not joined.
        drop(self.writer.take());
    }
}

impl Shared {
    fn secrets(&self) -> Arc<SecretFilter> {
        Arc::clone(
            &self
                .secrets
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn wanted(&self, level: &Level, target: &str) -> bool {
        self.wanted_by_file(level, target) || self.wanted_by_stderr(level)
    }

    /// Whether the filter lets the line through. It decides what the file
    /// takes, and which spans are timed, file or no file.
    fn wanted_by_file(&self, level: &Level, target: &str) -> bool {
        self.filter
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled(level, target)
    }

    fn wanted_by_stderr(&self, level: &Level) -> bool {
        level_number(LevelFilter::from_level(*level)) <= self.stderr_level.load(Ordering::Relaxed)
    }

    /// The spans a line is inside, outermost first, each with its fields.
    fn context(&self, innermost: Option<u64>) -> Vec<(&'static str, Map<String, Value>)> {
        let spans = crate::sync::lock(&self.spans);
        let mut context = Vec::new();
        let mut next = innermost;
        while let Some(id) = next {
            let Some(span) = spans.get(&id) else { break };
            context.push((span.metadata.name(), span.fields.clone()));
            next = span.parent;
        }
        context.reverse();
        context
    }

    fn write(
        &self,
        level: &Level,
        target: &str,
        message: &str,
        fields: &Map<String, Value>,
        innermost: Option<u64>,
    ) {
        let to_file = self.wanted_by_file(level, target);
        let to_stderr = self.wanted_by_stderr(level);
        if !to_file && !to_stderr {
            return;
        }
        let context = self.context(innermost);
        if to_file && let Some(file) = &self.file {
            let mut line = Map::new();
            line.insert(
                "ts".to_owned(),
                Value::from(self.clock.now().to_timestamp().as_str()),
            );
            line.insert("level".to_owned(), Value::from(level_name(level)));
            line.insert("target".to_owned(), Value::from(target));
            line.insert("msg".to_owned(), Value::from(message));
            if !fields.is_empty() {
                line.insert("fields".to_owned(), Value::Object(fields.clone()));
            }
            if !context.is_empty() {
                let spans = context
                    .iter()
                    .map(|(name, fields)| {
                        let mut span = Map::new();
                        span.insert("name".to_owned(), Value::from(*name));
                        for (key, value) in fields {
                            span.insert(key.clone(), value.clone());
                        }
                        Value::Object(span)
                    })
                    .collect();
                line.insert("spans".to_owned(), Value::Array(spans));
            }
            if let Ok(mut bytes) = serde_json::to_vec(&Value::Object(line)) {
                bytes.push(b'\n');
                file.send(bytes);
            }
        }
        if to_stderr {
            let mut line = format!("{}: {message}", level_name(level));
            for (key, value) in fields {
                let _ = write!(line, " {key}={}", plain(value));
            }
            let _ = write!(line, " ({target}");
            for (name, _) in &context {
                let _ = write!(line, " > {name}");
            }
            line.push_str(")\n");
            if let Some(stderr) = crate::sync::lock(&self.stderr).as_mut()
                && stderr.level >= *level
            {
                // Standard error that cannot be written has nowhere to say
                // so.
                let _ = stderr.out.write_all(line.as_bytes());
                let _ = stderr.out.flush();
            }
        }
    }
}

/// A field's value as a person reads it: text without its quotes.
fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Collects the fields of an event or a span, masked and cut.
struct Fields<'a> {
    secrets: &'a SecretFilter,
    message: Option<String>,
    fields: &'a mut Map<String, Value>,
}

impl Fields<'_> {
    fn text(&mut self, field: &Field, value: &str) {
        let masked = self.secrets.redact(value);
        let mut kept: String = masked.into_owned();
        if kept.len() > MAX_VALUE {
            let mut cut = MAX_VALUE;
            while !kept.is_char_boundary(cut) {
                cut -= 1;
            }
            let dropped = kept.len() - cut;
            kept.truncate(cut);
            let _ = write!(kept, "… [{dropped} more bytes]");
        }
        if field.name() == "message" {
            self.message = Some(kept);
        } else {
            self.fields
                .insert(field.name().to_owned(), Value::from(kept));
        }
    }
}

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.text(field, &format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.text(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .insert(field.name().to_owned(), Value::from(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.text(field, &value.to_string());
    }
}

fn current() -> Option<u64> {
    ENTERED.with(|entered| entered.borrow().last().copied())
}

impl tracing::Subscriber for LogSubscriber {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if self.enabled(metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // A span is tracked when it is wanted by a sink, like an event:
        // the spans the probes read are opened at `info`.
        self.shared.wanted(metadata.level(), metadata.target())
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        let file = self
            .shared
            .filter
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .most_detailed();
        let stderr = crate::sync::lock(&self.shared.stderr)
            .as_ref()
            .map_or(LevelFilter::OFF, |stderr| stderr.level);
        Some(file.max(stderr))
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        let shared = &self.shared;
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        let parent = if attributes.is_contextual() {
            current()
        } else {
            attributes.parent().map(Id::into_u64)
        };
        let mut fields = Map::new();
        let secrets = shared.secrets();
        attributes.record(&mut Fields {
            secrets: &secrets,
            message: None,
            fields: &mut fields,
        });
        crate::sync::lock(&shared.spans).insert(
            id,
            OpenSpan {
                metadata: attributes.metadata(),
                parent,
                fields,
                opened: shared.clock.monotonic(),
                busy: Duration::ZERO,
                entered: None,
                handles: 1,
            },
        );
        Id::from_u64(id)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        let mut fields = Map::new();
        let secrets = self.shared.secrets();
        values.record(&mut Fields {
            secrets: &secrets,
            message: None,
            fields: &mut fields,
        });
        if let Some(open) = crate::sync::lock(&self.shared.spans).get_mut(&span.into_u64()) {
            open.fields.extend(fields);
        }
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let shared = &self.shared;
        let metadata = event.metadata();
        let mut fields = Map::new();
        let secrets = shared.secrets();
        let mut visitor = Fields {
            secrets: &secrets,
            message: None,
            fields: &mut fields,
        };
        event.record(&mut visitor);
        let message = visitor.message.unwrap_or_default();
        let innermost = if event.is_contextual() {
            current()
        } else {
            event.parent().map(Id::into_u64)
        };
        shared.write(
            metadata.level(),
            metadata.target(),
            &message,
            &fields,
            innermost,
        );
    }

    fn enter(&self, span: &Id) {
        let id = span.into_u64();
        ENTERED.with(|entered| entered.borrow_mut().push(id));
        let now = self.shared.clock.monotonic();
        if let Some(open) = crate::sync::lock(&self.shared.spans).get_mut(&id) {
            open.entered = Some(match open.entered {
                Some((since, depth)) => (since, depth + 1),
                None => (now, 1),
            });
        }
    }

    fn exit(&self, span: &Id) {
        let id = span.into_u64();
        ENTERED.with(|entered| {
            let mut entered = entered.borrow_mut();
            if let Some(at) = entered.iter().rposition(|open| *open == id) {
                entered.remove(at);
            }
        });
        let now = self.shared.clock.monotonic();
        if let Some(open) = crate::sync::lock(&self.shared.spans).get_mut(&id) {
            open.entered = match open.entered {
                Some((since, 1)) => {
                    open.busy += now.saturating_sub(since);
                    None
                }
                Some((since, depth)) => Some((since, depth - 1)),
                None => None,
            };
        }
    }

    fn clone_span(&self, id: &Id) -> Id {
        if let Some(open) = crate::sync::lock(&self.shared.spans).get_mut(&id.into_u64()) {
            open.handles += 1;
        }
        id.clone()
    }

    fn try_close(&self, id: Id) -> bool {
        let shared = &self.shared;
        let id = id.into_u64();
        let closed = {
            let mut spans = crate::sync::lock(&shared.spans);
            let Some(open) = spans.get_mut(&id) else {
                return false;
            };
            open.handles = open.handles.saturating_sub(1);
            if open.handles > 0 {
                return false;
            }
            spans.remove(&id)
        };
        let Some(open) = closed else { return false };
        let now = shared.clock.monotonic();
        let busy = match open.entered {
            Some((since, _)) => open.busy + now.saturating_sub(since),
            None => open.busy,
        };
        let timing = SpanTiming {
            id,
            parent: open.parent,
            name: open.metadata.name(),
            target: open.metadata.target(),
            fields: open
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), plain(value)))
                .collect(),
            opened: open.opened,
            closed: now,
            busy,
        };
        let mut fields = open.fields;
        fields.insert("span".to_owned(), Value::from(timing.name));
        fields.insert("elapsed_ms".to_owned(), millis(timing.elapsed()));
        fields.insert("busy_ms".to_owned(), millis(timing.busy));
        shared.write(
            open.metadata.level(),
            open.metadata.target(),
            "span closed",
            &fields,
            open.parent,
        );
        shared.timings.push(timing);
        true
    }

    fn current_span(&self) -> Current {
        let Some(id) = current() else {
            return Current::none();
        };
        match crate::sync::lock(&self.shared.spans).get(&id) {
            Some(open) => Current::new(Id::from_u64(id), open.metadata),
            None => Current::none(),
        }
    }
}

/// A duration in milliseconds, with the fraction a probe needs.
fn millis(duration: Duration) -> Value {
    let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a span's length in microseconds is far below 2^52"
    )]
    Value::from(micros as f64 / 1000.0)
}

#[cfg(test)]
mod tests;
