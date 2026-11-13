//! The `read` tool (`docs/architecture/05-tools.md`, "`read`").
//!
//! A file is read once, as a stream: the same pass hashes it, counts its
//! lines, and keeps the lines that were asked for, so a large file never has
//! to fit in memory. What comes back depends on what the model already has
//! (`docs/architecture/15-efficiency.md`, "Changed-only re-reads"): the
//! lines, one line saying the file is unchanged, or a diff.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "reading a file blocks; the tool runs this on the blocking pool"
)]

use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use cortex_base::{AbsPath, CancelToken, ContentHash, ContentHasher, PathResolver, ResolvedPath};
use cortex_protocol::ids::ArtifactId;
use cortex_protocol::message::{ImagePart, Part};
use serde::Deserialize;
use serde_json::json;

use crate::args;
use crate::blocking::run_blocking;
use crate::context::{ImageSupport, Outliner, PlanCx, RunCx};
use crate::contract::{BoxFuture, Effect, Json, Plan, Tool, ToolOutput, ToolSpec};
use crate::detect::{self, Kind};
use crate::diff;
use crate::files::{FileSnapshot, FileTable, LineRanges, Shown};
use crate::output::{Budget, OutputLimits, count, size};
use crate::result::{ToolError, ToolResult};
use crate::store::{ContentStore, StoreError};

/// The largest file whose content is kept as a version to diff against. A
/// larger file that changes is sent as content.
const KEPT_VERSION_BYTES: u64 = 1024 * 1024;

/// What the header and the closing notes may take of the output limits.
const RESERVED_LINES: usize = 4;
const RESERVED_BYTES: usize = 768;

/// How much is read from a file at a time.
const CHUNK_BYTES: usize = 64 * 1024;

/// How many names of a directory are looked through for close matches.
const MAX_NAMES: usize = 10_000;

/// The `read` tool.
#[derive(Debug)]
pub struct ReadTool {
    spec: ToolSpec,
}

impl ReadTool {
    /// The tool.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: ToolSpec::new(
                "read",
                "Read a text file, or part of one. The result is a header line (the path, the \
                 lines returned, the total number of lines) and then the lines, exactly as they \
                 are in the file, with no line numbers in front. Give `offset` and `limit` to \
                 read a range: `search` results carry line numbers. A file too long to return \
                 whole comes back as its first lines with a note saying where to continue. \
                 Reading a file you have already read returns one line if it has not changed, \
                 or a diff if little has; `fresh: true` returns the content regardless. To read \
                 the stored full output of an earlier call, give its id as `artifact` in place \
                 of `path`.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "minLength": 1,
                            "description": "The file: relative to the workspace, absolute, or starting with ~."
                        },
                        "artifact": {
                            "type": "string",
                            "minLength": 1,
                            "description": "An artifact id from the notice of a cut result, in place of `path`."
                        },
                        "offset": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "The first line to return, counted from 1."
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "How many lines to return."
                        },
                        "fresh": {
                            "type": "boolean",
                            "description": "Return the content even if you have already read it."
                        }
                    },
                    "additionalProperties": false
                }),
            ),
        }
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: Option<String>,
    artifact: Option<String>,
    offset: Option<u64>,
    limit: Option<u64>,
    #[serde(default)]
    fresh: bool,
}

/// What a call reads.
#[derive(Debug, Clone)]
enum Source {
    File(ResolvedPath),
    Artifact(ArtifactId),
}

/// A validated call.
#[derive(Debug, Clone)]
struct Call {
    source: Source,
    /// Whether the model named a range.
    ranged: bool,
    /// The first line wanted.
    first: u64,
    /// The last line wanted. `u64::MAX` for "to the end".
    last: u64,
    fresh: bool,
}

/// One of `path` and `artifact`, and never both: what `read` and `search`
/// both take.
pub(crate) fn one_source<'a>(
    tool: &str,
    path: Option<&'a str>,
    artifact: Option<&'a str>,
) -> Result<Result<&'a str, ArtifactId>, ToolError> {
    match (path, artifact) {
        (Some(_), Some(_)) => Err(ToolError::invalid(format!(
            "`{tool}` takes `path` or `artifact`, not both: a file, or the stored output of an \
             earlier call."
        ))),
        (Some(path), None) => Ok(Ok(path)),
        (None, Some(artifact)) => Ok(Err(ArtifactId::new(artifact.trim()))),
        (None, None) => Err(ToolError::invalid(format!(
            "`{tool}` needs `path` (a file) or `artifact` (the id of a stored output)."
        ))),
    }
}

impl Tool for ReadTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        let arguments: Arguments = args::parse(&self.spec, args)?;
        let first = arguments.offset.unwrap_or(1);
        let last = match arguments.limit {
            Some(limit) => first.saturating_add(limit - 1),
            None => u64::MAX,
        };
        let ranged = arguments.offset.is_some() || arguments.limit.is_some();
        let range = match (ranged, last) {
            (false, _) => String::new(),
            (true, u64::MAX) => format!(", from line {first}"),
            (true, last) if last == first => format!(", line {first}"),
            (true, last) => format!(", lines {first}-{last}"),
        };
        let source = one_source(
            "read",
            arguments.path.as_deref(),
            arguments.artifact.as_deref(),
        )?;
        let call = |source| Call {
            source,
            ranged,
            first,
            last,
            fresh: arguments.fresh,
        };
        match source {
            Ok(path) => {
                let resolved = cx.paths.resolve(path)?;
                Ok(Plan::new(
                    format!("Read {}{range}", resolved.display()),
                    call(Source::File(resolved.clone())),
                )
                .with_effect(Effect::Read(resolved)))
            }
            // An artifact is the session's own record of what a tool already
            // returned: reading it is no effect on anything outside.
            Err(id) => Ok(Plan::new(
                format!("Read artifact {id}{range}"),
                call(Source::Artifact(id)),
            )),
        }
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
            let env = Env {
                paths: Arc::clone(&cx.paths),
                store: Arc::clone(&cx.store),
                files: Arc::clone(&cx.files),
                limits: cx.limits,
                outliner: cx.outliner.clone(),
                images: cx.images.clone(),
                turn: cx.turn,
            };
            let files = Arc::clone(&cx.files);
            let done = run_blocking(&cx.cancel, move |cancel| {
                let mut seen = None;
                let result = match &call.source {
                    Source::File(path) => read_file(path, &call, &env, cancel, &mut seen),
                    Source::Artifact(id) => read_artifact(id, &call, &env, cancel),
                };
                (result, seen)
            })
            .await;
            match done {
                Ok((result, seen)) => {
                    // What the model has seen is recorded here, with the
                    // result in hand, and not by the blocking work: a call
                    // cancelled at the last moment returns "interrupted",
                    // and must not leave the table saying the lines were
                    // shown.
                    if let Some((path, snapshot, shown)) = seen {
                        files.record_read(&path, snapshot, shown);
                    }
                    result
                }
                Err(error) => args::unfinished(error, "read"),
            }
        })
    }
}

/// What a read needs of the session, owned so that it can cross to the
/// blocking pool.
struct Env {
    paths: Arc<PathResolver>,
    store: Arc<dyn ContentStore>,
    files: Arc<FileTable>,
    limits: OutputLimits,
    outliner: Option<Arc<dyn Outliner>>,
    images: Option<ImageSupport>,
    turn: u32,
}

/// The text a file turned out to hold.
struct Scan {
    hash: ContentHash,
    size: u64,
    total_lines: u64,
    /// The wanted lines that fit, numbered.
    lines: Vec<(u64, String)>,
    /// The numbers of the lines that are shown shortened, over the line
    /// limit. The model has not seen them in full.
    shortened: Vec<u64>,
    /// The first wanted line that did not fit the output limits.
    stopped_at: Option<u64>,
    /// Whether a line shown was not valid UTF-8.
    invalid: bool,
    /// The whole content, when it is small enough to keep as a version.
    full: Option<Vec<u8>>,
}

enum Scanned {
    Text(Scan),
    NotText(Kind),
    Cancelled,
}

/// Collects the wanted lines of a stream as it goes by.
struct Lines {
    limits: OutputLimits,
    budget: Budget,
    first: u64,
    last: u64,
    /// The number of the line being read, counted from 1.
    number: u64,
    /// Whether the line being read has any content yet.
    open: bool,
    /// Whether the last byte of the line so far is a carriage return.
    ends_with_cr: bool,
    /// The start of the line being read, if it is wanted.
    kept: Vec<u8>,
    line_bytes: usize,
    line_chars: usize,
    lines: Vec<(u64, String)>,
    shortened: Vec<u64>,
    stopped_at: Option<u64>,
    invalid: bool,
}

impl Lines {
    fn new(limits: OutputLimits, first: u64, last: u64) -> Self {
        Self {
            limits,
            budget: limits.budget(RESERVED_LINES, RESERVED_BYTES),
            first,
            last,
            number: 1,
            open: false,
            ends_with_cr: false,
            kept: Vec::new(),
            line_bytes: 0,
            line_chars: 0,
            lines: Vec::new(),
            shortened: Vec::new(),
            stopped_at: None,
            invalid: false,
        }
    }

    fn wanted(&self) -> bool {
        self.stopped_at.is_none() && self.first <= self.number && self.number <= self.last
    }

    fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            match bytes.iter().position(|&byte| byte == b'\n') {
                Some(end) => {
                    self.piece(&bytes[..end]);
                    self.end_line();
                    bytes = &bytes[end + 1..];
                }
                None => {
                    self.piece(bytes);
                    bytes = &[];
                }
            }
        }
    }

    fn piece(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.open = true;
        self.ends_with_cr = bytes.last() == Some(&b'\r');
        if !self.wanted() {
            return;
        }
        // No more of a line is kept than the line limit can show: a line of
        // a hundred megabytes costs a few kilobytes.
        let room = (self.limits.max_line_chars() * 4 + 8).saturating_sub(self.kept.len());
        self.kept.extend_from_slice(&bytes[..bytes.len().min(room)]);
        self.line_bytes += bytes.len();
        // A character is one byte that is not a continuation byte.
        self.line_chars += bytes.iter().filter(|&&byte| byte & 0xc0 != 0x80).count();
    }

    fn end_line(&mut self) {
        if self.wanted() {
            let whole = self.line_bytes == self.kept.len();
            let mut invalid = false;
            let mut bytes = self.kept.as_slice();
            let mut chars = self.line_chars;
            // The carriage return of a CRLF ending is not part of the line,
            // whether or not the line was short enough to keep whole.
            if self.ends_with_cr {
                chars -= 1;
                if whole {
                    bytes = &bytes[..bytes.len() - 1];
                }
            }
            if self.number == 1 && bytes.starts_with(b"\xef\xbb\xbf") {
                bytes = &bytes[3..];
                chars -= 1;
            }
            let text = match std::str::from_utf8(bytes) {
                Ok(text) => std::borrow::Cow::Borrowed(text),
                // A line cut in the middle of a character is not a fault of
                // the file.
                Err(error) if !whole && error.error_len().is_none() => {
                    String::from_utf8_lossy(&bytes[..error.valid_up_to()])
                }
                Err(_) => {
                    invalid = true;
                    String::from_utf8_lossy(bytes)
                }
            };
            let (text, shortened) = if whole {
                match self.limits.clip(&text) {
                    std::borrow::Cow::Borrowed(text) => (text.to_owned(), false),
                    std::borrow::Cow::Owned(text) => (text, true),
                }
            } else {
                (self.limits.clip_start(&text, chars), true)
            };
            if self.budget.take(&text) {
                self.lines.push((self.number, text));
                if shortened {
                    self.shortened.push(self.number);
                }
                // Only a line that is shown can make the result say the
                // file is not UTF-8.
                self.invalid |= invalid;
            } else {
                self.stopped_at = Some(self.number);
            }
        }
        self.number += 1;
        self.open = false;
        self.ends_with_cr = false;
        self.kept.clear();
        self.line_bytes = 0;
        self.line_chars = 0;
    }

    /// The end of the stream: a last line with no line break after it is a
    /// line all the same.
    fn finish(&mut self) -> u64 {
        if self.open {
            self.end_line();
        }
        self.number - 1
    }
}

/// Reads a stream to its end: hashes it, counts its lines, keeps the ones
/// wanted, and keeps all of it if it is small.
fn scan(
    reader: &mut dyn Read,
    call: &Call,
    limits: OutputLimits,
    cancel: &CancelToken,
) -> io::Result<Scanned> {
    let mut buffer = vec![0u8; CHUNK_BYTES];
    // Enough of the start to tell what the stream is.
    let mut filled = 0;
    while filled < detect::SNIFF_BYTES {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let kind = detect::sniff(&buffer[..filled]);
    if kind != Kind::Text {
        return Ok(Scanned::NotText(kind));
    }

    let mut hasher = ContentHasher::new();
    let mut lines = Lines::new(limits, call.first, call.last);
    let mut full = Some(Vec::new());
    let mut size = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Ok(Scanned::Cancelled);
        }
        let chunk = &buffer[..filled];
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
        size += chunk.len() as u64;
        if size > KEPT_VERSION_BYTES {
            full = None;
        } else if let Some(full) = full.as_mut() {
            full.extend_from_slice(chunk);
        }
        lines.feed(chunk);
        filled = loop {
            match reader.read(&mut buffer) {
                Ok(read) => break read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        };
    }
    let total_lines = lines.finish();
    Ok(Scanned::Text(Scan {
        hash: hasher.finish(),
        size,
        total_lines,
        lines: lines.lines,
        shortened: lines.shortened,
        stopped_at: lines.stopped_at,
        invalid: lines.invalid,
        full,
    }))
}

/// What a read adds to the table of seen files, once its result is
/// delivered.
type Seen = (AbsPath, FileSnapshot, Shown);

fn read_file(
    path: &ResolvedPath,
    call: &Call,
    env: &Env,
    cancel: &CancelToken,
    seen: &mut Option<Seen>,
) -> ToolResult {
    let shown = path.display();
    let fs_path = path.to_fs_path();
    let metadata = match std::fs::symlink_metadata(&fs_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return missing(path, env),
        Err(error) => return ToolResult::error(format!("`{shown}` could not be read: {error}.")),
    };
    if metadata.is_dir() {
        return ToolResult::error(format!(
            "`{shown}` is a directory. Use `list` to see what is in it."
        ));
    }
    if !metadata.is_file() {
        return ToolResult::error(format!(
            "`{shown}` is not a regular file (it is a device, a pipe or a socket), so it is not \
             read."
        ));
    }
    let mut file = match std::fs::File::open(&fs_path) {
        Ok(file) => file,
        Err(error) => return ToolResult::error(format!("`{shown}` could not be read: {error}.")),
    };
    let scan = match scan(&mut file, call, env.limits, cancel) {
        Ok(Scanned::Text(scan)) => scan,
        Ok(Scanned::NotText(kind)) => return not_text(path, &fs_path, metadata.len(), kind, env),
        Ok(Scanned::Cancelled) => {
            return ToolResult::interrupted(format!("Reading `{shown}` was interrupted."));
        }
        Err(error) => return ToolResult::error(format!("`{shown}` could not be read: {error}.")),
    };
    if let Some(past_end) = past_end(&shown, call, &scan) {
        return past_end;
    }

    let abs = path.abs();
    let snapshot = FileSnapshot {
        size: scan.size,
        modified: metadata.modified().ok(),
        hash: scan.hash,
    };
    let first = scan.lines.first().map_or(1, |(number, _)| *number);
    let last = scan.lines.last().map_or(0, |(number, _)| *number);
    let earlier = env.files.shown(abs);
    let outline = if !call.ranged && scan.stopped_at.is_some() {
        env.outliner
            .as_ref()
            .and_then(|outliner| outliner.outline(abs))
    } else {
        None
    };
    let content = content_text(&shown, &scan, call, outline.as_deref(), &env.limits);

    if let Some(prior) = earlier.as_ref().filter(|_| !call.fresh) {
        if prior.hash == scan.hash {
            if prior.lines.covers(first, last) {
                *seen = Some((abs.clone(), snapshot, prior.clone()));
                let text = if prior.lines.is_empty() {
                    format!(
                        "{shown}: unchanged since you read it{}: it is still empty.",
                        when(prior.turn)
                    )
                } else {
                    format!(
                        "{shown}: unchanged since you read lines {}{} ({} in all). The content \
                         is not sent again: pass `fresh: true` if you need it.",
                        prior.lines,
                        when(prior.turn),
                        lines_word(scan.total_lines),
                    )
                };
                return ToolResult::success(text).with_payload(json!({
                    "path": shown,
                    "kind": "unchanged",
                    "total_lines": scan.total_lines,
                }));
            }
        } else if let Some((text, after, added, removed)) = changed_text(&shown, prior, &scan, env)
            // A diff answers the call only if, with it, the model has seen
            // every line the call would have returned. The lines are those
            // of the file as it is now: lines that moved into the range
            // asked for, and were never shown, are not in the diff.
            && after.covers(first, last)
            && text.len() < content.text.len()
            && env.limits.fits(&text)
        {
            let kept = keep_version(&scan, env);
            *seen = Some((
                abs.clone(),
                snapshot,
                Shown {
                    hash: scan.hash,
                    lines: after,
                    total_lines: scan.total_lines,
                    turn: env.turn,
                    kept,
                },
            ));
            return ToolResult::success(text).with_payload(json!({
                "path": shown,
                "kind": "diff",
                "total_lines": scan.total_lines,
                "added": added,
                "removed": removed,
            }));
        }
    }

    // The content. What the model has now is what it had of this version,
    // and the lines just sent: those sent whole. A line shown shortened has
    // not been seen, and a later read of it must not be answered with
    // "unchanged".
    let mut lines = match &earlier {
        Some(prior) if prior.hash == scan.hash => prior.lines.clone(),
        _ => LineRanges::new(),
    };
    let mut from = content.first;
    for &number in scan
        .shortened
        .iter()
        .filter(|number| (content.first..=content.last).contains(number))
    {
        lines.insert(from, number - 1);
        from = number + 1;
    }
    lines.insert(from, content.last);
    let kept = keep_version(&scan, env);
    *seen = Some((
        abs.clone(),
        snapshot,
        Shown {
            hash: scan.hash,
            lines,
            total_lines: scan.total_lines,
            turn: env.turn,
            kept,
        },
    ));
    ToolResult::success(content.text).with_payload(json!({
        "path": shown,
        "kind": if content.outlined { "outline" } else { "content" },
        "first_line": content.first,
        "last_line": content.last,
        "total_lines": scan.total_lines,
    }))
}

/// Keeps the version just read, if it is small enough, so that a later
/// read can diff against it. Whether it was kept.
fn keep_version(scan: &Scan, env: &Env) -> bool {
    scan.full
        .as_ref()
        .is_some_and(|content| env.store.put_version(content).is_ok())
}

/// ` in turn 3`, or nothing when the turn is not known.
fn when(turn: u32) -> String {
    if turn == 0 {
        String::new()
    } else {
        format!(" in turn {turn}")
    }
}

fn lines_word(lines: u64) -> String {
    if lines == 1 {
        "1 line".to_owned()
    } else {
        format!("{} lines", count(lines as usize))
    }
}

/// An error for an `offset` past the end of the file.
fn past_end(shown: &str, call: &Call, scan: &Scan) -> Option<ToolResult> {
    if call.first <= scan.total_lines || (call.first == 1 && scan.total_lines == 0) {
        return None;
    }
    Some(ToolResult::error(format!(
        "`{shown}` has {}, so there is no line {}. Read it without `offset`, or with a smaller \
         one.",
        lines_word(scan.total_lines),
        call.first
    )))
}

/// The result text for content, and which lines it shows.
struct Content {
    text: String,
    first: u64,
    last: u64,
    outlined: bool,
}

fn content_text(
    shown: &str,
    scan: &Scan,
    call: &Call,
    outline: Option<&str>,
    limits: &OutputLimits,
) -> Content {
    let total = scan.total_lines;
    let mut lines: &[(u64, String)] = &scan.lines;
    let mut text = String::new();
    let mut outlined = false;

    if let Some(outline) = outline.filter(|outline| !outline.trim().is_empty()) {
        // The outline first, in at most a quarter of the room, then as many
        // of the first lines as still fit.
        let mut budget = limits.budget(RESERVED_LINES + 2, RESERVED_BYTES + 256);
        let quarter_lines = budget.lines_left() / 4;
        let quarter_bytes = budget.bytes_left() / 4;
        let mut outline_lines = Vec::new();
        let mut used = 0;
        for line in outline.lines() {
            used += line.len() + 1;
            if outline_lines.len() >= quarter_lines || used > quarter_bytes || !budget.take(line) {
                break;
            }
            outline_lines.push(limits.clip(line));
        }
        let fitting = lines
            .iter()
            .take_while(|(_, line)| budget.take(line))
            .count();
        lines = &lines[..fitting];
        if !outline_lines.is_empty() {
            outlined = true;
            text.push_str(&format!(
                "{shown} ({}, {}) is too long to return whole. Its outline, with line numbers:\n",
                lines_word(total),
                size(scan.size as usize)
            ));
            text.push_str(&outline_lines.join("\n"));
            text.push('\n');
        }
    }

    let first = lines.first().map_or(1, |(number, _)| *number);
    let last = lines.last().map_or(0, |(number, _)| *number);
    let header = if total == 0 {
        format!("{shown} (empty: 0 lines)")
    } else if first == 1 && last == total {
        format!("{shown} ({})", lines_word(total))
    } else if lines.is_empty() {
        format!("{shown} (no lines shown of {})", count(total as usize))
    } else if first == last {
        format!("{shown} (line {first} of {})", count(total as usize))
    } else {
        format!(
            "{shown} (lines {first}-{last} of {})",
            count(total as usize)
        )
    };
    text.push_str(&header);
    for (_, line) in lines {
        text.push('\n');
        text.push_str(line);
    }

    // The lines wanted that are not shown: the output limits stopped the
    // read before the range, or the file, ended.
    let wanted_last = call.last.min(total);
    if last < wanted_last && (scan.stopped_at.is_some() || outlined) {
        let next = lines.last().map_or(call.first, |(number, _)| number + 1);
        text.push_str(&format!(
            "\n[Stopped before line {next}: the output limit was reached. {} not shown: \
             continue with `offset: {next}`.]",
            lines_word(wanted_last + 1 - next),
        ));
    }
    if scan.invalid {
        text.push_str(
            "\n[This file is not valid UTF-8. The bytes that are not are shown as \u{fffd}, and \
             the editing tools will refuse the file.]",
        );
    }
    Content {
        text,
        first,
        last,
        outlined,
    }
}

/// The result for a file that changed since the model saw it: a diff, if
/// one can be made. Returns the text, the lines of the new version the model
/// will have seen once it has the diff, and the counts of added and removed
/// lines. Whether the diff answers the call is for the caller to decide.
fn changed_text(
    shown: &str,
    prior: &Shown,
    scan: &Scan,
    env: &Env,
) -> Option<(String, LineRanges, usize, usize)> {
    if !prior.kept {
        return None;
    }
    let new = std::str::from_utf8(scan.full.as_deref()?).ok()?;
    let old = env.store.get_version(&prior.hash).ok()??;
    let old = std::str::from_utf8(&old).ok()?;
    let diff = diff::diff(old, new)?;
    let seen = diff.seen_after(&prior.lines);
    let text = if diff.is_empty() {
        format!(
            "{shown}: only its line endings, its byte-order mark or its final line break \
             changed since you read it{}. Its {} are the same.",
            when(prior.turn),
            lines_word(scan.total_lines)
        )
    } else {
        format!(
            "{shown}: changed since you read it{}, and now has {}. This is the difference \
             from the version you saw (+{} -{}); pass `fresh: true` for the content.\n{}",
            when(prior.turn),
            lines_word(scan.total_lines),
            diff.added,
            diff.removed,
            diff.text
        )
    };
    Some((text, seen, diff.added, diff.removed))
}

/// The result for a file that is not text: an image, if the model takes
/// it, and otherwise a refusal with the size and the detected type.
fn not_text(path: &ResolvedPath, fs_path: &Path, bytes: u64, kind: Kind, env: &Env) -> ToolResult {
    let shown = path.display();
    let human = size(usize::try_from(bytes).unwrap_or(usize::MAX));
    match kind {
        Kind::Image(media_type) => {
            let Some(images) = &env.images else {
                return ToolResult::error(format!(
                    "`{shown}` is an image ({media_type}, {human}), and the model in use does \
                     not take images. It cannot be read as text."
                ));
            };
            if !images.accepts(media_type, bytes) {
                return ToolResult::error(format!(
                    "`{shown}` is an image ({media_type}, {human}) that the model in use does \
                     not take: it takes {} of up to {}.",
                    images.media_types.join(", "),
                    size(usize::try_from(images.max_bytes).unwrap_or(usize::MAX))
                ));
            }
            let stored = std::fs::read(fs_path)
                .map_err(StoreError::from)
                .and_then(|content| env.store.put_artifact(&shown, media_type, &content));
            match stored {
                Ok(artifact) => ToolResult::success(format!("{shown} ({media_type}, {human})"))
                    .with_part(Part::Image(ImagePart {
                        media_type: media_type.to_owned(),
                        data: None,
                        artifact: Some(artifact.id.clone()),
                    }))
                    .with_artifact(artifact)
                    .with_payload(json!({"path": shown, "kind": "image", "bytes": bytes})),
                Err(error) => ToolResult::error(format!("`{shown}` could not be read: {error}.")),
            }
        }
        Kind::Binary(what) => ToolResult::error(format!(
            "`{shown}` is not a text file: it is {what}, {human}. It is not read."
        )),
        Kind::Text => ToolError::Internal("text was taken for binary".to_owned()).into_result(),
    }
}

fn read_artifact(id: &ArtifactId, call: &Call, env: &Env, cancel: &CancelToken) -> ToolResult {
    let shown = format!("artifact {id}");
    let mut reader = match env.store.open_artifact(id) {
        Ok(reader) => reader,
        Err(StoreError::NoSuchArtifact(_)) => {
            return ToolResult::error(format!(
                "There is no artifact `{id}` in this session. An artifact id is given in the \
                 notice at the end of a result that was cut."
            ));
        }
        Err(error) => return ToolResult::error(format!("The {shown} could not be read: {error}.")),
    };
    let scan = match scan(reader.as_mut(), call, env.limits, cancel) {
        Ok(Scanned::Text(scan)) => scan,
        Ok(Scanned::NotText(Kind::Image(media_type))) => {
            return ToolResult::error(format!("The {shown} is an image ({media_type}), not text."));
        }
        Ok(Scanned::NotText(_)) => {
            return ToolResult::error(format!("The {shown} is not text, so it is not read."));
        }
        Ok(Scanned::Cancelled) => {
            return ToolResult::interrupted(format!("Reading the {shown} was interrupted."));
        }
        Err(error) => return ToolResult::error(format!("The {shown} could not be read: {error}.")),
    };
    if let Some(past_end) = past_end(&shown, call, &scan) {
        return past_end;
    }
    let content = content_text(&shown, &scan, call, None, &env.limits);
    ToolResult::success(content.text).with_payload(json!({
        "artifact": id.as_str(),
        "kind": "content",
        "first_line": content.first,
        "last_line": content.last,
        "total_lines": scan.total_lines,
    }))
}

/// The error for a file that is not there, with the names in its directory
/// that are close to the one asked for.
fn missing(path: &ResolvedPath, env: &Env) -> ToolResult {
    let shown = path.display();
    // The deepest directory on the way that does exist, and the first name
    // under it that does not.
    let mut wanted = path.abs().clone();
    let mut parent = wanted.parent();
    while let Some(candidate) = &parent {
        if std::fs::symlink_metadata(candidate.to_fs_path()).is_ok_and(|found| found.is_dir()) {
            break;
        }
        wanted = candidate.clone();
        parent = wanted.parent();
    }
    let (Some(parent), Some(name)) = (parent, wanted.file_name()) else {
        return ToolResult::error(format!("There is no file at `{shown}`."));
    };
    let directory = args::shown(&env.paths, &parent);
    let whole = &wanted == path.abs();
    let what = if whole {
        format!("There is no file at `{shown}`.")
    } else {
        format!(
            "There is no file at `{shown}`: `{}` does not exist.",
            args::shown(&env.paths, &wanted)
        )
    };
    let close = close_matches(&parent, name);
    if close.is_empty() {
        ToolResult::error(format!(
            "{what} Nothing in `{directory}` has a similar name: `list` it to see what is \
             there."
        ))
    } else {
        ToolResult::error(format!(
            "{what} Close matches in `{directory}`: {}.",
            close.join(", ")
        ))
    }
}

/// The names in `directory` closest to `wanted`, best first, at most five.
fn close_matches(directory: &AbsPath, wanted: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(directory.to_fs_path()) else {
        return Vec::new();
    };
    let wanted_lower = wanted.to_lowercase();
    let wanted_stem = stem(&wanted_lower);
    let allowed = (wanted.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, String)> = entries
        .take(MAX_NAMES)
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            let lower = name.to_lowercase();
            let score = if lower == wanted_lower {
                0
            } else {
                let distance = edit_distance(&lower, &wanted_lower, allowed);
                if distance <= allowed {
                    distance
                } else if stem(&lower) == wanted_stem && !wanted_stem.is_empty() {
                    // The same name with another extension.
                    allowed + 1
                } else if wanted_stem.chars().count() >= 3
                    && (lower.starts_with(wanted_stem) || wanted_lower.starts_with(stem(&lower)))
                    && stem(&lower).chars().count() >= 3
                {
                    allowed + 2
                } else {
                    return None;
                }
            };
            Some((score, name))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(5).map(|(_, name)| name).collect()
}

/// A file name without its last extension.
fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// How many single-character edits turn one text into the other, or any
/// number above `limit` when it is more than `limit`.
fn edit_distance(first: &str, second: &str, limit: usize) -> usize {
    let first: Vec<char> = first.chars().collect();
    let second: Vec<char> = second.chars().collect();
    if first.len().abs_diff(second.len()) > limit {
        return limit + 1;
    }
    let mut row: Vec<usize> = (0..=second.len()).collect();
    for (i, a) in first.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in second.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if a == b {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[second.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(first: u64, last: u64) -> Call {
        Call {
            source: Source::Artifact(ArtifactId::new("a_0001")),
            ranged: first != 1 || last != u64::MAX,
            first,
            last,
            fresh: false,
        }
    }

    fn scan_text(text: &[u8], first: u64, last: u64, limits: OutputLimits) -> Scan {
        let mut reader = text;
        match scan(&mut reader, &call(first, last), limits, &CancelToken::new()).expect("no I/O") {
            Scanned::Text(scan) => scan,
            _ => panic!("the text was not read as text"),
        }
    }

    fn numbers(scan: &Scan) -> Vec<u64> {
        scan.lines.iter().map(|(number, _)| *number).collect()
    }

    fn texts(scan: &Scan) -> Vec<&str> {
        scan.lines.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn lines_are_counted_whether_or_not_the_last_one_ends() {
        let limits = OutputLimits::default();
        for (text, expected) in [
            ("", 0),
            ("a", 1),
            ("a\n", 1),
            ("a\nb", 2),
            ("a\nb\n", 2),
            ("\n", 1),
            ("\n\n", 2),
            ("a\n\nb\n", 3),
        ] {
            let scan = scan_text(text.as_bytes(), 1, u64::MAX, limits);
            assert_eq!(scan.total_lines, expected, "{text:?}");
            assert_eq!(scan.lines.len() as u64, expected, "{text:?}");
            assert_eq!(scan.size, text.len() as u64);
            assert_eq!(scan.hash, ContentHash::of(text.as_bytes()));
            assert_eq!(scan.full.as_deref(), Some(text.as_bytes()));
        }
    }

    #[test]
    fn a_range_is_kept_and_the_rest_is_only_counted() {
        let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let scan = scan_text(text.as_bytes(), 10, 12, OutputLimits::default());
        assert_eq!(texts(&scan), ["line 10", "line 11", "line 12"]);
        assert_eq!(numbers(&scan), [10, 11, 12]);
        assert_eq!(scan.total_lines, 100);
        assert_eq!(scan.stopped_at, None);
    }

    #[test]
    fn line_endings_and_a_byte_order_mark_are_not_part_of_a_line() {
        let scan = scan_text(
            b"\xef\xbb\xbfone\r\ntwo\r\nthree",
            1,
            u64::MAX,
            OutputLimits::default(),
        );
        assert_eq!(texts(&scan), ["one", "two", "three"]);
        assert!(!scan.invalid);
        // A carriage return inside a line is part of it.
        let scan = scan_text(b"a\rb\n", 1, u64::MAX, OutputLimits::default());
        assert_eq!(texts(&scan), ["a\rb"]);
    }

    #[test]
    fn the_output_limits_stop_the_lines_and_say_where() {
        let limits = OutputLimits::new(1 << 20, 20, 2000);
        let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let scan = scan_text(text.as_bytes(), 1, u64::MAX, limits);
        assert_eq!(scan.lines.len(), 20 - RESERVED_LINES);
        assert_eq!(scan.stopped_at, Some(17));
        assert_eq!(scan.total_lines, 100, "the rest is still counted");
    }

    #[test]
    fn a_very_long_line_is_shortened_without_being_held_whole() {
        let limits = OutputLimits::new(1 << 20, 100, 300);
        let mut text = "é".repeat(100_000);
        text.push_str("\nshort\n");
        let scan = scan_text(text.as_bytes(), 1, u64::MAX, limits);
        let (kept, marker) = scan.lines[0].1.rsplit_once("… [+").expect("a marker");
        assert!(kept.chars().all(|c| c == 'é'), "cut between characters");
        assert_eq!(
            marker,
            format!("{} characters]", count(100_000 - kept.chars().count()))
        );
        assert!(scan.lines[0].1.chars().count() <= 300);
        assert_eq!(scan.lines[1].1, "short");
        assert!(!scan.invalid);
    }

    #[test]
    fn bytes_that_are_not_utf8_are_shown_as_replacements_and_reported() {
        let scan = scan_text(b"caf\xe9\nplain\n", 1, u64::MAX, OutputLimits::default());
        assert_eq!(texts(&scan), ["caf\u{fffd}", "plain"]);
        assert!(scan.invalid);
        // Outside the lines shown, nobody looks.
        let scan = scan_text(b"caf\xe9\nplain\n", 2, 2, OutputLimits::default());
        assert!(!scan.invalid);
    }

    #[test]
    fn content_over_a_megabyte_is_not_kept_as_a_version() {
        let text = vec![b'x'; KEPT_VERSION_BYTES as usize + 1];
        let scan = scan_text(&text, 1, 1, OutputLimits::default());
        assert!(scan.full.is_none());
        assert_eq!(scan.size, KEPT_VERSION_BYTES + 1);
        assert_eq!(scan.hash, ContentHash::of(&text));
        let text = vec![b'x'; KEPT_VERSION_BYTES as usize];
        assert!(
            scan_text(&text, 1, 1, OutputLimits::default())
                .full
                .is_some()
        );
    }

    #[test]
    fn binary_content_is_not_scanned() {
        let mut reader: &[u8] = b"\x7fELF\x02\x01\x01\0rest";
        let scanned = scan(
            &mut reader,
            &call(1, u64::MAX),
            OutputLimits::default(),
            &CancelToken::new(),
        );
        assert!(matches!(
            scanned,
            Ok(Scanned::NotText(Kind::Binary("an ELF executable")))
        ));
    }

    #[test]
    fn a_cancelled_scan_stops() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut reader: &[u8] = b"text\n";
        let scanned = scan(
            &mut reader,
            &call(1, u64::MAX),
            OutputLimits::default(),
            &cancel,
        );
        assert!(matches!(scanned, Ok(Scanned::Cancelled)));
    }

    #[test]
    fn edit_distance_counts_edits_up_to_a_limit() {
        assert_eq!(edit_distance("main.rs", "main.rs", 3), 0);
        assert_eq!(edit_distance("mian.rs", "main.rs", 3), 2);
        assert_eq!(edit_distance("main.r", "main.rs", 3), 1);
        assert_eq!(edit_distance("", "abc", 3), 3);
        assert!(edit_distance("short", "a much longer name", 3) > 3);
    }

    #[test]
    fn a_stem_is_a_name_without_its_last_extension() {
        assert_eq!(stem("main.rs"), "main");
        assert_eq!(stem("archive.tar.gz"), "archive.tar");
        assert_eq!(stem("Makefile"), "Makefile");
        assert_eq!(stem(".gitignore"), ".gitignore");
    }
}
