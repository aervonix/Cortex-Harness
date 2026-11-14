//! The `search` tool (`docs/architecture/05-tools.md`, "`search`"): file
//! contents searched with a regular expression, in-process, with the
//! ripgrep libraries (`docs/decisions/0014-search-in-process.md`).
//!
//! The walk is parallel and finds files in no particular order, and the
//! result must be the same every time. So every thread searches the files it
//! is handed and merges what it found into one table kept in path order,
//! which holds only as many matches as the result can show: the rest are
//! counted, not kept. A pattern that matches everywhere in a large tree
//! costs time, and no more memory than a pattern that matches once.

#![allow(
    clippy::disallowed_methods,
    reason = "searching files blocks; the tool runs this on the blocking pool"
)]

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::{CancelToken, ResolvedPath};
use cortex_protocol::ids::ArtifactId;
use grep_matcher::LineTerminator;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkFinish, SinkMatch,
};
use serde::Deserialize;
use serde_json::json;

use crate::args;
use crate::blocking::run_blocking;
use crate::context::{PlanCx, ReadGate, RunCx};
use crate::contract::{BoxFuture, Effect, Json, Plan, Tool, ToolOutput, ToolSpec};
use crate::output::{OutputLimits, count};
use crate::read::one_source;
use crate::result::{ToolError, ToolResult};
use crate::store::{ContentStore, StoreError};
use crate::walk::{self, EntryKind, GlobFilter, Skipped, WalkSpec};

/// How many matches are returned unless the call says otherwise.
const DEFAULT_RESULTS: u64 = 100;
/// The most matches a call may ask for.
const MAX_RESULTS: u64 = 2000;
/// The most lines of context around a match.
const MAX_CONTEXT: u64 = 5;

/// What the header and the closing notes may take of the output limits.
const RESERVED_LINES: usize = 8;
const RESERVED_BYTES: usize = 1536;

/// The `search` tool.
#[derive(Debug)]
pub struct SearchTool {
    spec: ToolSpec,
    threads: usize,
}

impl SearchTool {
    /// The tool, walking with as many threads as the walker chooses for the
    /// machine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: ToolSpec::new(
                "search",
                "Search the contents of files with a regular expression (Rust regex syntax), \
                 one line at a time. `path` is a directory, searched with everything under it, \
                 or one file; it defaults to the workspace root. `glob` keeps only files that \
                 match, for example `*.rs` or `src/**/*.ts`. Results are grouped by file, in \
                 path order, each line with its line number, which `read` takes as `offset`. \
                 `mode: \"files\"` returns only the names of files with a match, and \
                 `mode: \"count\"` the number of matching lines in each. Files that ignore \
                 files (.gitignore, .ignore) leave out, hidden files and binary files are not \
                 searched. To search the stored full output of an earlier call, give its id as \
                 `artifact` in place of `path`.",
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "minLength": 1,
                            "description": "The regular expression. It is matched within a line."
                        },
                        "path": {
                            "type": "string",
                            "minLength": 1,
                            "description": "A directory or a file. Defaults to the workspace root."
                        },
                        "artifact": {
                            "type": "string",
                            "minLength": 1,
                            "description": "An artifact id from the notice of a cut result, in place of `path`."
                        },
                        "glob": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Only files that match, for example *.rs."
                        },
                        "ignore_case": {
                            "type": "boolean",
                            "description": "Match letters whatever their case."
                        },
                        "context": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": MAX_CONTEXT,
                            "description": "Lines to show before and after each match. 0 unless given."
                        },
                        "mode": {
                            "enum": ["lines", "files", "count"],
                            "description": "lines: the matching lines (the default). files: the names of files with a match. count: matching lines per file."
                        },
                        "max_results": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_RESULTS,
                            "description": "The most matches (or files) to return. 100 unless given."
                        }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }),
            ),
            threads: 0,
        }
    }

    /// Sets how many threads walk and search. 0, the default, lets the
    /// walker choose for the machine.
    #[must_use]
    pub fn threads(mut self, threads: usize) -> Self {
        self.threads = threads;
        self
    }
}

impl Default for SearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    pattern: String,
    path: Option<String>,
    artifact: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    ignore_case: bool,
    context: Option<u64>,
    mode: Option<Mode>,
    max_results: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Lines,
    Files,
    Count,
}

#[derive(Debug, Clone)]
enum Target {
    Path(ResolvedPath),
    Artifact(ArtifactId),
}

/// A validated call.
#[derive(Debug, Clone)]
struct Call {
    pattern: String,
    matcher: RegexMatcher,
    target: Target,
    glob: Option<(String, GlobFilter)>,
    context: usize,
    mode: Mode,
    max_results: usize,
    threads: usize,
}

impl Tool for SearchTool {
    fn preview(&self, _plan: &Plan, _cx: &RunCx) -> Option<crate::Preview> {
        // It changes no file.
        None
    }

    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        let arguments: Arguments = args::parse(&self.spec, args)?;
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(arguments.ignore_case)
            // `^` and `$` are the start and end of a line, with either
            // line ending, and a match never spans two lines.
            .multi_line(true)
            .crlf(true)
            .ban_byte(Some(0))
            .build(&arguments.pattern)
            .map_err(|error| {
                ToolError::invalid(format!(
                    "`pattern` is not a regular expression this can search with: {error}. A \
                     pattern is matched within one line, in Rust regex syntax; to find text as \
                     it is written, put a backslash before `.`, `(`, `[`, `{{`, `*`, `+`, `?`, \
                     `|`, `^`, `$` and `\\`."
                ))
            })?;
        let glob = arguments
            .glob
            .map(|pattern| GlobFilter::new(&pattern).map(|filter| (pattern, filter)))
            .transpose()?;
        let (target, effect, place) =
            match (arguments.path.as_deref(), arguments.artifact.as_deref()) {
                (None, None) => in_path(".", cx)?,
                (path, artifact) => match one_source("search", path, artifact)? {
                    Ok(path) => in_path(path, cx)?,
                    Err(id) => {
                        let place = format!("artifact {id}");
                        (Target::Artifact(id), None, place)
                    }
                },
            };
        let call = Call {
            matcher,
            target,
            glob,
            context: usize::try_from(arguments.context.unwrap_or(0)).unwrap_or(0),
            mode: arguments.mode.unwrap_or(Mode::Lines),
            max_results: usize::try_from(arguments.max_results.unwrap_or(DEFAULT_RESULTS))
                .unwrap_or(usize::MAX),
            threads: self.threads,
            pattern: arguments.pattern,
        };
        let mut plan = Plan::new(format!("Search {place} for {}", call.pattern), call);
        plan.effects.extend(effect);
        Ok(plan)
    }

    fn run<'a>(
        &'a self,
        plan: Plan,
        cx: &'a RunCx,
        _out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let call = match plan.into_args::<Call>() {
                Ok(call) => call,
                Err(error) => return error.into_result(),
            };
            let gate = Arc::clone(&cx.gate);
            let store = Arc::clone(&cx.store);
            let limits = cx.limits;
            run_blocking(&cx.cancel, move |cancel| {
                search(&call, gate.as_ref(), store.as_ref(), &limits, cancel)
            })
            .await
            .unwrap_or_else(|error| args::unfinished(error, "search"))
        })
    }
}

/// Resolves the path of a search and declares its effect: a walk of the
/// tree for a directory, a read for a file.
fn in_path(path: &str, cx: &PlanCx) -> Result<(Target, Option<Effect>, String), ToolError> {
    let resolved = cx.paths.resolve(path)?;
    let is_dir = std::fs::symlink_metadata(resolved.to_fs_path()).is_ok_and(|found| found.is_dir());
    let effect = if is_dir {
        Effect::ReadTree(resolved.clone())
    } else {
        Effect::Read(resolved.clone())
    };
    let place = resolved.display();
    Ok((Target::Path(resolved), Some(effect), place))
}

/// One line of a file's results, as the result will show it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hit {
    /// A matching line: `  12: text`.
    Match(String),
    /// A line of context: `  11- text`.
    Context(String),
    /// A gap between two groups of lines.
    Break,
}

/// What a search found in one file.
#[derive(Debug, Default)]
struct FileHits {
    hits: Vec<Hit>,
    /// How many of `hits` are matches.
    kept: usize,
    /// How many lines matched in all.
    matches: u64,
}

/// Collects one file's matches: as many as the result could show, and a
/// count of the rest.
struct FileSink<'a> {
    mode: Mode,
    keep: usize,
    limits: OutputLimits,
    cancel: &'a CancelToken,
    found: FileHits,
    /// How many bytes of lines are kept. No result can show more than the
    /// output limit, so no more than that is kept of one file.
    kept_bytes: usize,
    /// A match was counted without being kept. Nothing after it is kept.
    overflowed: bool,
    binary: bool,
}

impl FileSink<'_> {
    /// A line as the result shows it: its number, a sign (`:` for a match,
    /// `-` for context) and its text, shortened if it is over the line
    /// limit, with a marker that counts what is missing from the whole line.
    fn line(&mut self, number: Option<u64>, sign: char, bytes: &[u8]) -> String {
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        let prefix = format!("  {}{sign} ", number.unwrap_or(0));
        // No more of a line is looked at than the line limit can show: a
        // minified file is one line of megabytes. A character is one byte
        // that is not a continuation byte.
        let room = self.limits.max_line_chars() * 4 + 8;
        let line = if bytes.len() <= room {
            let text = String::from_utf8_lossy(bytes);
            self.limits.clip(&format!("{prefix}{text}")).into_owned()
        } else {
            let chars = bytes.iter().filter(|&&byte| byte & 0xc0 != 0x80).count();
            let text = String::from_utf8_lossy(&bytes[..room]);
            self.limits
                .clip_start(&format!("{prefix}{text}"), prefix.chars().count() + chars)
        };
        self.kept_bytes += line.len() + 1;
        if self.kept_bytes > self.limits.max_bytes() {
            self.overflowed = true;
        }
        line
    }
}

impl Sink for FileSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.cancel.is_cancelled() {
            return Ok(false);
        }
        self.found.matches += 1;
        match self.mode {
            // One match is enough to name the file. The search stops here, so
            // a NUL byte further on is never met: a file can be named in this
            // mode and left out as binary by the modes that read on.
            Mode::Files => return Ok(false),
            Mode::Count => {}
            Mode::Lines => {
                if self.found.kept < self.keep && !self.overflowed {
                    let line = self.line(found.line_number(), ':', found.bytes());
                    self.found.hits.push(Hit::Match(line));
                    self.found.kept += 1;
                } else {
                    self.overflowed = true;
                }
            }
        }
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        context: &SinkContext<'_>,
    ) -> Result<bool, io::Error> {
        if self.mode == Mode::Lines && !self.overflowed {
            let line = self.line(context.line_number(), '-', context.bytes());
            self.found.hits.push(Hit::Context(line));
        }
        Ok(true)
    }

    fn context_break(&mut self, _searcher: &Searcher) -> Result<bool, io::Error> {
        if self.mode == Mode::Lines && !self.overflowed {
            self.found.hits.push(Hit::Break);
        }
        Ok(true)
    }

    fn finish(&mut self, _searcher: &Searcher, finish: &SinkFinish) -> Result<(), io::Error> {
        self.binary = finish.binary_byte_offset().is_some();
        Ok(())
    }
}

/// What a search found, kept in path order and bounded.
#[derive(Debug, Default)]
struct Merged {
    files: BTreeMap<String, FileHits>,
    /// Matches kept across `files`.
    kept: usize,
    /// Matching lines in all, kept or not.
    matches: u64,
    /// Files with a match in all, kept or not.
    matched_files: u64,
}

impl Merged {
    /// Adds one file's results, then lets go of whatever the result could
    /// no longer show: files that sort after enough matches to fill it.
    fn add(&mut self, path: String, hits: FileHits, mode: Mode, max_results: usize) {
        self.matches += hits.matches;
        self.matched_files += 1;
        self.kept += hits.kept;
        self.files.insert(path, hits);
        while let Some((_, last)) = self.files.last_key_value() {
            let surplus = match mode {
                Mode::Lines => self.files.len() > 1 && self.kept - last.kept >= max_results,
                Mode::Files | Mode::Count => self.files.len() > max_results,
            };
            if !surplus {
                break;
            }
            if let Some((_, dropped)) = self.files.pop_last() {
                self.kept -= dropped.kept;
            }
        }
    }
}

fn searcher(call: &Call) -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .line_terminator(LineTerminator::crlf())
        .before_context(call.context)
        .after_context(call.context)
        // A file with a NUL byte is binary, and is not searched.
        .binary_detection(BinaryDetection::quit(0))
        .build()
}

fn sink<'a>(call: &Call, limits: &OutputLimits, cancel: &'a CancelToken) -> FileSink<'a> {
    FileSink {
        mode: call.mode,
        keep: call.max_results,
        limits: *limits,
        cancel,
        found: FileHits::default(),
        kept_bytes: 0,
        overflowed: false,
        binary: false,
    }
}

/// The outcome of searching one named file or artifact.
enum Single {
    Found(FileHits),
    Binary,
    Failed(String),
}

fn search_one_file(
    call: &Call,
    path: &Path,
    limits: &OutputLimits,
    cancel: &CancelToken,
) -> Single {
    let mut sink = sink(call, limits, cancel);
    match searcher(call).search_path(&call.matcher, path, &mut sink) {
        Ok(()) if sink.binary => Single::Binary,
        Ok(()) => Single::Found(sink.found),
        Err(error) => Single::Failed(error.to_string()),
    }
}

fn search(
    call: &Call,
    gate: &dyn ReadGate,
    store: &dyn ContentStore,
    limits: &OutputLimits,
    cancel: &CancelToken,
) -> ToolResult {
    let mut merged = Merged::default();
    let mut skipped = Skipped::default();
    let place;
    match &call.target {
        Target::Artifact(id) => {
            place = format!("artifact {id}");
            let reader = match store.open_artifact(id) {
                Ok(reader) => reader,
                Err(StoreError::NoSuchArtifact(_)) => {
                    return ToolResult::error(format!(
                        "There is no artifact `{id}` in this session. An artifact id is given \
                         in the notice at the end of a result that was cut."
                    ));
                }
                Err(error) => {
                    return ToolResult::error(format!("The {place} could not be read: {error}."));
                }
            };
            let mut sink = sink(call, limits, cancel);
            if let Err(error) = searcher(call).search_reader(&call.matcher, reader, &mut sink) {
                return ToolResult::error(format!("The {place} could not be read: {error}."));
            }
            if sink.binary {
                return ToolResult::error(format!(
                    "The {place} is not text, so it is not searched."
                ));
            }
            if sink.found.matches > 0 {
                merged.add(place.clone(), sink.found, call.mode, call.max_results);
            }
        }
        Target::Path(path) => {
            place = path.display();
            let fs_path = path.to_fs_path();
            match std::fs::symlink_metadata(&fs_path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return ToolResult::error(format!(
                        "There is nothing at `{place}` to search. `list` its parent to see what \
                         is there."
                    ));
                }
                Err(error) => {
                    return ToolResult::error(format!("`{place}` could not be searched: {error}."));
                }
                Ok(metadata) if metadata.is_dir() => {
                    skipped = search_tree(call, path, gate, limits, cancel, &mut merged);
                }
                Ok(metadata) if metadata.is_file() => {
                    match search_one_file(call, &fs_path, limits, cancel) {
                        Single::Found(hits) if hits.matches > 0 => {
                            merged.add(place.clone(), hits, call.mode, call.max_results);
                        }
                        Single::Found(_) => {}
                        Single::Binary => {
                            return ToolResult::error(format!(
                                "`{place}` is a binary file, so it is not searched."
                            ));
                        }
                        Single::Failed(error) => {
                            return ToolResult::error(format!(
                                "`{place}` could not be searched: {error}."
                            ));
                        }
                    }
                }
                Ok(_) => {
                    return ToolResult::error(format!(
                        "`{place}` is not a regular file or a directory, so it is not searched."
                    ));
                }
            }
        }
    }
    if cancel.is_cancelled() {
        return ToolResult::interrupted(format!(
            "The search of `{place}` was interrupted before it finished."
        ));
    }
    render(call, &place, &merged, &skipped, limits)
}

/// Searches every file under a directory, on the walker's threads.
fn search_tree(
    call: &Call,
    root: &ResolvedPath,
    gate: &dyn ReadGate,
    limits: &OutputLimits,
    cancel: &CancelToken,
    merged: &mut Merged,
) -> Skipped {
    let root_shown = root.display();
    let shared = Mutex::new(std::mem::take(merged));
    let extra = Mutex::new(Skipped::default());
    let mut skipped = walk::walk(
        &WalkSpec {
            root,
            max_depth: None,
            include_ignored: false,
            glob: call.glob.as_ref().map(|(_, filter)| filter),
            threads: call.threads,
        },
        gate,
        cancel,
        &mut || {
            let (shared, extra, root_shown) = (&shared, &extra, &root_shown);
            let mut searcher = searcher(call);
            Box::new(move |entry: &walk::Found<'_>| {
                match entry.kind {
                    EntryKind::Dir => return,
                    EntryKind::Link => {
                        extra.lock().unwrap_or_else(PoisonError::into_inner).links += 1;
                        return;
                    }
                    EntryKind::File => {}
                }
                let mut sink = sink(call, limits, cancel);
                match searcher.search_path(&call.matcher, entry.fs_path, &mut sink) {
                    Ok(()) => {
                        if !sink.binary && sink.found.matches > 0 {
                            shared.lock().unwrap_or_else(PoisonError::into_inner).add(
                                walk::shown_path(root_shown, entry.relative),
                                sink.found,
                                call.mode,
                                call.max_results,
                            );
                        }
                    }
                    Err(error) => {
                        extra
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .unreadable(&format!(
                                "{}: {error}",
                                walk::shown_path(root_shown, entry.relative)
                            ));
                    }
                }
            })
        },
    );
    *merged = shared.into_inner().unwrap_or_else(PoisonError::into_inner);
    skipped.absorb(extra.into_inner().unwrap_or_else(PoisonError::into_inner));
    skipped
}

fn plural(number: u64, one: &str, many: &str) -> String {
    let number_text = count(usize::try_from(number).unwrap_or(usize::MAX));
    if number == 1 {
        format!("{number_text} {one}")
    } else {
        format!("{number_text} {many}")
    }
}

/// Writes the result: a header with the totals, the matches grouped by file
/// in path order, and a note for whatever is not shown.
fn render(
    call: &Call,
    place: &str,
    merged: &Merged,
    skipped: &Skipped,
    limits: &OutputLimits,
) -> ToolResult {
    let scope = match &call.glob {
        Some((glob, _)) => format!("`{place}` (files matching `{glob}`)"),
        None => format!("`{place}`"),
    };
    let payload = |shown: u64| {
        json!({
            "matches": merged.matches,
            "files": merged.matched_files,
            "shown": shown,
        })
    };
    if merged.matched_files == 0 {
        let mut text = format!("No matches for `{}` in {scope}.", call.pattern);
        for note in skipped.notes() {
            text.push('\n');
            text.push_str(&note);
        }
        return ToolResult::success(text).with_payload(payload(0));
    }

    let mut budget = limits.budget(RESERVED_LINES, RESERVED_BYTES);
    let mut body: Vec<String> = Vec::new();
    // How many matches (or, by file, how many files) the body shows.
    let mut shown = 0u64;
    let mut out_of_room = false;
    'files: for (path, file) in &merged.files {
        match call.mode {
            Mode::Files | Mode::Count => {
                if shown as usize >= call.max_results {
                    break;
                }
                let line = match call.mode {
                    Mode::Count => format!("{path}: {}", file.matches),
                    _ => path.clone(),
                };
                let line = limits.clip(&line).into_owned();
                if !budget.take(&line) {
                    out_of_room = true;
                    break;
                }
                body.push(line);
                shown += 1;
            }
            Mode::Lines => {
                // A file's name is shown only with one of its matches under
                // it, and lines of context only with the match they are
                // around: `waiting` holds what has no match yet.
                let mut waiting = vec![limits.clip(path).into_owned()];
                let mut named = false;
                for hit in &file.hits {
                    let line = match hit {
                        Hit::Match(line) | Hit::Context(line) => line.clone(),
                        Hit::Break => "  --".to_owned(),
                    };
                    waiting.push(line);
                    if !matches!(hit, Hit::Match(..)) {
                        continue;
                    }
                    if shown as usize >= call.max_results {
                        break 'files;
                    }
                    if !waiting.iter().all(|line| budget.take(line)) {
                        out_of_room = true;
                        break 'files;
                    }
                    body.append(&mut waiting);
                    named = true;
                    shown += 1;
                }
                // The context after the file's last match, as far as it fits.
                if named {
                    while waiting.last().is_some_and(|line| line == "  --") {
                        waiting.pop();
                    }
                    for line in waiting {
                        if !budget.take(&line) {
                            break;
                        }
                        body.push(line);
                    }
                }
            }
        }
    }

    let (total, header) = match call.mode {
        Mode::Files => {
            let total = merged.matched_files;
            let header = if shown == total {
                format!(
                    "{} with a match for `{}` in {scope}:",
                    plural(total, "file", "files"),
                    call.pattern
                )
            } else {
                format!(
                    "The first {} of {} with a match for `{}` in {scope}:",
                    count(shown as usize),
                    plural(total, "file", "files"),
                    call.pattern
                )
            };
            (total, header)
        }
        Mode::Count => {
            let total = merged.matched_files;
            let all = format!(
                "{} in {} for `{}` in {scope}",
                plural(merged.matches, "matching line", "matching lines"),
                plural(total, "file", "files"),
                call.pattern
            );
            let header = if shown == total {
                format!("{all}:")
            } else {
                format!("{all}. The first {} files:", count(shown as usize))
            };
            (total, header)
        }
        Mode::Lines => {
            let total = merged.matches;
            let all = format!(
                "{} in {} for `{}` in {scope}",
                plural(total, "matching line", "matching lines"),
                plural(merged.matched_files, "file", "files"),
                call.pattern
            );
            let header = if shown == total {
                format!("{all}:")
            } else {
                format!("{all}. The first {}:", count(shown as usize))
            };
            (total, header)
        }
    };

    let mut text = header;
    for line in &body {
        text.push('\n');
        text.push_str(line);
    }
    if shown < total {
        let reason = if out_of_room {
            "the output limit was reached"
        } else {
            "`max_results` was reached"
        };
        let more = match call.mode {
            Mode::Lines => plural(total - shown, "more matching line", "more matching lines"),
            Mode::Files | Mode::Count => plural(total - shown, "more file", "more files"),
        };
        text.push_str(&format!(
            "\n[{more} not shown: {reason}. Narrow `pattern`, `path` or `glob` to see them."
        ));
        if call.mode == Mode::Lines {
            text.push_str(" `mode: \"count\"` shows where they are.");
        }
        text.push(']');
    }
    for note in skipped.notes() {
        text.push('\n');
        text.push_str(&note);
    }
    ToolResult::success(text).with_payload(payload(shown))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(matches: usize) -> FileHits {
        FileHits {
            hits: (1..=matches as u64)
                .map(|n| Hit::Match(format!("  {n}: match {n}")))
                .collect(),
            kept: matches,
            matches: matches as u64,
        }
    }

    #[test]
    fn the_merged_table_keeps_the_first_files_in_path_order_whatever_order_they_arrive_in() {
        let mut merged = Merged::default();
        for name in ["d.rs", "b.rs", "e.rs", "a.rs", "c.rs"] {
            merged.add(name.to_owned(), hits(2), Mode::Lines, 5);
        }
        // Five matches fill the result: a, b and the first of c. Later
        // files are counted and let go.
        assert_eq!(
            merged.files.keys().collect::<Vec<_>>(),
            ["a.rs", "b.rs", "c.rs"]
        );
        assert_eq!(merged.kept, 6);
        assert_eq!(merged.matches, 10);
        assert_eq!(merged.matched_files, 5);
    }

    #[test]
    fn one_file_with_more_matches_than_fit_is_still_kept() {
        let mut merged = Merged::default();
        merged.add("only.rs".to_owned(), hits(50), Mode::Lines, 5);
        assert_eq!(merged.files.len(), 1);
        merged.add("z.rs".to_owned(), hits(1), Mode::Lines, 5);
        assert_eq!(merged.files.keys().collect::<Vec<_>>(), ["only.rs"]);
        assert_eq!(merged.matches, 51);
    }

    #[test]
    fn by_file_the_table_keeps_as_many_files_as_may_be_shown() {
        for mode in [Mode::Files, Mode::Count] {
            let mut merged = Merged::default();
            for name in ["d", "b", "e", "a", "c"] {
                merged.add(name.to_owned(), hits(3), mode, 2);
            }
            assert_eq!(merged.files.keys().collect::<Vec<_>>(), ["a", "b"]);
            assert_eq!(merged.matched_files, 5);
            assert_eq!(merged.matches, 15);
        }
    }
}
