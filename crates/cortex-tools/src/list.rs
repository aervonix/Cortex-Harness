//! The `list` tool (`docs/architecture/05-tools.md`, "`list`"): the entries
//! of a directory, or the files under it that match a glob.

#![allow(
    clippy::disallowed_methods,
    reason = "listing a directory blocks; the tool runs this on the blocking pool"
)]

use std::collections::BTreeSet;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::{CancelToken, PathResolver, ResolvedPath};
use serde::Deserialize;
use serde_json::json;

use crate::args;
use crate::blocking::run_blocking;
use crate::context::{PlanCx, ReadGate, RunCx};
use crate::contract::{BoxFuture, Effect, Json, Plan, Tool, ToolOutput, ToolSpec};
use crate::output::{OutputLimits, count};
use crate::result::{ToolError, ToolResult};
use crate::walk::{self, EntryKind, GlobFilter, Skipped, WalkSpec};

/// How many entries are returned unless the call says otherwise.
const DEFAULT_RESULTS: u64 = 200;
/// The most entries a call may ask for.
const MAX_RESULTS: u64 = 5000;

/// What the header and the closing notes may take of the output limits.
const RESERVED_LINES: usize = 8;
const RESERVED_BYTES: usize = 1536;

/// The `list` tool.
#[derive(Debug)]
pub struct ListTool {
    spec: ToolSpec,
}

impl ListTool {
    /// The tool.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: ToolSpec::new(
                "list",
                "List a directory, or find files by name. Without `glob` it returns the \
                 directory's own entries; with one, such as `**/*.rs` or `*.toml`, every entry \
                 under the directory that matches, at any depth. A glob with no `/` matches a \
                 name anywhere; one with a `/` matches the path below the directory. Paths come \
                 back one per line, sorted, ready to pass to other tools, and a directory ends \
                 with `/`. Files that ignore files (.gitignore, .ignore) leave out, and hidden \
                 files, are not listed unless `include_ignored` is set.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "minLength": 1,
                            "description": "The directory. Defaults to the workspace root."
                        },
                        "glob": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Only entries that match, for example **/*.rs."
                        },
                        "depth": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "How many levels down to go. 1 without a glob; no limit with one."
                        },
                        "max_results": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_RESULTS,
                            "description": "The most entries to return. 200 unless given."
                        },
                        "include_ignored": {
                            "type": "boolean",
                            "description": "Also list hidden files and files that ignore files leave out."
                        }
                    },
                    "additionalProperties": false
                }),
            ),
        }
    }
}

impl Default for ListTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: Option<String>,
    glob: Option<String>,
    depth: Option<u64>,
    max_results: Option<u64>,
    #[serde(default)]
    include_ignored: bool,
}

/// A validated call.
#[derive(Debug, Clone)]
struct Call {
    directory: ResolvedPath,
    glob: Option<(String, GlobFilter)>,
    depth: Option<usize>,
    max_results: usize,
    include_ignored: bool,
}

impl Tool for ListTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        let arguments: Arguments = args::parse(&self.spec, args)?;
        let directory = cx.paths.resolve(arguments.path.as_deref().unwrap_or("."))?;
        let glob = arguments
            .glob
            .map(|pattern| GlobFilter::new(&pattern).map(|filter| (pattern, filter)))
            .transpose()?;
        let depth = match (arguments.depth, &glob) {
            (Some(depth), _) => Some(usize::try_from(depth).unwrap_or(usize::MAX)),
            (None, None) => Some(1),
            (None, Some(_)) => None,
        };
        let shown = directory.display();
        let summary = match &glob {
            Some((pattern, _)) => format!("Find {pattern} under {shown}"),
            None => format!("List {shown}"),
        };
        // One level is the directory's own names. More is a walk of the
        // tree under it.
        let effect = if depth == Some(1) {
            Effect::Read(directory.clone())
        } else {
            Effect::ReadTree(directory.clone())
        };
        let call = Call {
            directory,
            glob,
            depth,
            max_results: usize::try_from(arguments.max_results.unwrap_or(DEFAULT_RESULTS))
                .unwrap_or(usize::MAX),
            include_ignored: arguments.include_ignored,
        };
        Ok(Plan::new(summary, call).with_effect(effect))
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
            let paths = Arc::clone(&cx.paths);
            let gate = Arc::clone(&cx.gate);
            let limits = cx.limits;
            run_blocking(&cx.cancel, move |cancel| {
                list(&call, &paths, gate.as_ref(), &limits, cancel)
            })
            .await
            .unwrap_or_else(|error| args::unfinished(error, "list"))
        })
    }
}

fn list(
    call: &Call,
    paths: &PathResolver,
    gate: &dyn ReadGate,
    limits: &OutputLimits,
    cancel: &CancelToken,
) -> ToolResult {
    let shown = call.directory.display();
    match std::fs::symlink_metadata(call.directory.to_fs_path()) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return ToolResult::error(format!(
                "`{shown}` is a file, not a directory. Use `read` to see what is in it."
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return ToolResult::error(format!(
                "There is no directory at `{shown}`. `list` its parent to see what is there."
            ));
        }
        Err(error) => {
            return ToolResult::error(format!("`{shown}` could not be listed: {error}."));
        }
    }

    // The first `max_results` entries in path order are all a result can
    // show. They are kept as the walk goes, and the rest are only counted,
    // so a listing of a very large tree costs no more memory than a small
    // one.
    let found = Mutex::new((BTreeSet::<String>::new(), 0usize));
    let refused_links = Mutex::new(Skipped::default());
    let inside = call.directory.is_inside_workspace();
    let mut skipped = walk::walk(
        &WalkSpec {
            root: &call.directory,
            max_depth: call.depth,
            include_ignored: call.include_ignored,
            glob: call.glob.as_ref().map(|(_, filter)| filter),
            threads: 0,
        },
        gate,
        cancel,
        &mut || {
            let (found, refused_links, shown) = (&found, &refused_links, &shown);
            Box::new(move |entry: &walk::Found<'_>| {
                let mut line = walk::shown_path(shown, entry.relative);
                let is_dir = match entry.kind {
                    EntryKind::Dir => true,
                    EntryKind::File => false,
                    // A link is listed as what it leads to, which the resolver
                    // finds by its own rules, without following anything it
                    // refuses. The walk does not go into it.
                    EntryKind::Link => {
                        // A path inside the workspace is relative, and is
                        // marked as such: a link named `~` is not the home
                        // directory.
                        let written = if inside {
                            format!("./{line}")
                        } else {
                            line.clone()
                        };
                        match paths.resolve(&written) {
                            Ok(target) => std::fs::symlink_metadata(target.to_fs_path())
                                .is_ok_and(|metadata| metadata.is_dir()),
                            Err(error) => {
                                refused_links
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .refuse(error.to_string());
                                return;
                            }
                        }
                    }
                };
                if is_dir {
                    line.push('/');
                }
                let mut found = found.lock().unwrap_or_else(PoisonError::into_inner);
                found.1 += 1;
                found.0.insert(line);
                if found.0.len() > call.max_results {
                    found.0.pop_last();
                }
            })
        },
    );
    if cancel.is_cancelled() {
        return ToolResult::interrupted(format!("Listing `{shown}` was interrupted."));
    }
    skipped.absorb(
        refused_links
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner),
    );

    let (entries, total) = found.into_inner().unwrap_or_else(PoisonError::into_inner);
    let mut budget = limits.budget(RESERVED_LINES, RESERVED_BYTES);
    let mut lines: Vec<String> = Vec::new();
    let mut out_of_room = false;
    for entry in &entries {
        let line = limits.clip(entry);
        if !budget.take(&line) {
            out_of_room = true;
            break;
        }
        lines.push(line.into_owned());
    }
    let listed = lines.len();

    let what = match &call.glob {
        Some((pattern, _)) => format!("matching `{pattern}` under `{shown}`"),
        None => format!("in `{shown}`"),
    };
    let entries_word = |number: usize| if number == 1 { "entry" } else { "entries" };
    let header = if total == 0 {
        let hint = if call.include_ignored {
            ""
        } else {
            " Hidden and ignored files are left out: `include_ignored: true` lists them too."
        };
        match &call.glob {
            Some((pattern, _)) => format!("Nothing under `{shown}` matches `{pattern}`.{hint}"),
            None => format!("`{shown}` is empty.{hint}"),
        }
    } else if listed == total {
        format!("{} {} {what}:", count(total), entries_word(total))
    } else {
        format!(
            "The first {} of {} {} {what}:",
            count(listed),
            count(total),
            entries_word(total)
        )
    };
    let mut text = header;
    for line in &lines {
        text.push('\n');
        text.push_str(line);
    }
    if listed < total {
        let reason = if out_of_room {
            "the output limit was reached"
        } else {
            "`max_results` was reached"
        };
        text.push_str(&format!(
            "\n[{} more not shown: {reason}. Narrow `path` or `glob` to see them.]",
            count(total - listed)
        ));
    }
    for note in skipped.notes() {
        text.push('\n');
        text.push_str(&note);
    }
    ToolResult::success(text).with_payload(json!({
        "path": shown,
        "entries": total,
        "shown": listed,
    }))
}
