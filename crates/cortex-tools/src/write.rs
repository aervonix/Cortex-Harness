//! The `write` tool (`docs/architecture/05-tools.md`, "`write`").
//!
//! It creates a file, or replaces the whole content of one that the model
//! has read. The steps, and what each protects, are in [`crate::change`].
//!
//! **What is written.** A new file gets the content exactly as the model
//! gave it. Over an existing file, the content keeps that file's line
//! endings and its byte-order mark: a file with CRLF endings stays one,
//! and in a file with both kinds of ending, the lines the new content has
//! in common with the old keep the ending each had.

use cortex_base::{CancelToken, ResolvedPath};
use serde::Deserialize;
use serde_json::json;

use crate::args;
use crate::blocking::run_blocking;
use crate::change::{Env, Found, Target, payload_diff};
use crate::checkpoint::Editing;
use crate::context::{PlanCx, RunCx};
use crate::contract::{
    BoxFuture, Concurrency, ConcurrencyClass, Effect, Json, Plan, Tool, ToolOutput, ToolSpec,
};
use crate::diff;
use crate::matching;
use crate::output::count;
use crate::result::{ToolError, ToolResult};
use crate::textfile::{BOM, Text};

/// The `write` tool.
#[derive(Debug)]
pub struct WriteTool {
    spec: ToolSpec,
    editing: Editing,
}

impl WriteTool {
    /// The tool, recording its changes and taking its waits as `editing`
    /// says.
    #[must_use]
    pub fn new(editing: Editing) -> Self {
        Self {
            spec: ToolSpec::new(
                "write",
                "Create a file, or replace the whole content of one. Missing parent \
                 directories are created. A file that already exists must have been read in \
                 this session and not have changed since, or the call fails and says to read \
                 it first. To change part of a file, use `edit`: it sends less. The result is \
                 a one-line confirmation with the number of lines written.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "minLength": 1,
                            "description": "The file: relative to the workspace, absolute, or starting with ~."
                        },
                        "content": {
                            "type": "string",
                            "description": "The full new content of the file."
                        }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            )
            .concurrency(ConcurrencyClass::Keyed),
            editing,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    content: String,
}

/// A validated call.
#[derive(Debug, Clone)]
struct Call {
    path: ResolvedPath,
    content: String,
}

/// Plans a call that changes one file: refuses a path that names a
/// directory, and declares the write and the key the call is scheduled by.
pub(crate) fn plan_change<T: Send + Sync + 'static>(
    summary: String,
    path: &ResolvedPath,
    call: T,
) -> Result<Plan, ToolError> {
    if path.names_a_directory() {
        return Err(ToolError::invalid(format!(
            "`{}` names a directory (it ends with a separator), and a file is needed.",
            path.display()
        )));
    }
    Ok(Plan::new(summary, call)
        .with_effect(Effect::Write(path.clone()))
        .with_concurrency(Concurrency::Keyed(vec![path.abs().clone()])))
}

/// Runs the blocking work of a change to its end. The work is not tied to
/// the call's token: a change that has begun is finished, and the work
/// asks the token itself before it writes anything.
pub(crate) async fn run_change(
    cx: &RunCx,
    tool: &'static str,
    work: impl FnOnce() -> ToolResult + Send + 'static,
) -> ToolResult {
    if cx.cancel.is_cancelled() {
        return ToolResult::interrupted(format!(
            "`{tool}` was interrupted before it started. Nothing was changed."
        ));
    }
    match run_blocking(&CancelToken::new(), move |_| work()).await {
        Ok(result) => result,
        Err(error) => args::unfinished(error, tool),
    }
}

impl Tool for WriteTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        let arguments: Arguments = args::parse(&self.spec, args)?;
        let path = cx.paths.resolve(&arguments.path)?;
        let lines = Text::new(&arguments.content).lines.len();
        plan_change(
            format!("Write {} ({})", path.display(), lines_word(lines)),
            &path,
            Call {
                path: path.clone(),
                content: arguments.content,
            },
        )
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
            let env = Env::new(cx, &self.editing);
            run_change(cx, "write", move || {
                write_file(&call, &env).unwrap_or_else(|refusal| refusal)
            })
            .await
        })
    }
}

fn write_file(call: &Call, env: &Env) -> Result<ToolResult, ToolResult> {
    let target = Target::new(&call.path, &env.paths)?;
    let shown = &target.shown;
    let found = target.find(env)?;
    let (old_text, new_bytes) = match &found {
        // A new file is the content, exactly as it was given.
        Found::Missing => (None, call.content.clone().into_bytes()),
        Found::File(existing) => {
            let (marked, old_text) = target.text(existing)?;
            target.guard(env, existing, "overwrite")?;
            let content = call.content.strip_prefix(BOM).unwrap_or(&call.content);
            let body = matching::rewrite(&Text::new(old_text), content);
            let mut new = String::with_capacity(body.len() + BOM.len());
            if marked {
                new.push_str(BOM);
            }
            new.push_str(&body);
            if new.as_bytes() == existing.bytes {
                return Ok(ToolResult::success(format!(
                    "`{shown}` already has exactly this content. Nothing was written."
                ))
                .with_payload(json!({"path": shown, "kind": "unchanged"})));
            }
            (Some(old_text), new.into_bytes())
        }
    };
    target.commit(env, &found, &new_bytes)?;

    // The content came in as a string, and only line endings and a mark
    // were added to it.
    let new_text = String::from_utf8_lossy(&new_bytes);
    let lines = Text::new(new_text.strip_prefix(BOM).unwrap_or(&new_text))
        .lines
        .len();
    let changed = diff::diff_bounded(old_text.unwrap_or(""), &new_text);
    let (diff_text, omitted) = payload_diff(changed.as_ref());
    let text = match (&found, &changed) {
        (Found::Missing, _) => format!("Created `{shown}`: {}.", lines_word(lines)),
        (Found::File(_), Some(diff)) => format!(
            "Wrote `{shown}`: {} ({} added, {} removed).",
            lines_word(lines),
            count(diff.added),
            count(diff.removed)
        ),
        (Found::File(_), None) => format!("Wrote `{shown}`: {}.", lines_word(lines)),
    };
    Ok(ToolResult::success(text).with_payload(json!({
        "path": shown,
        "kind": if old_text.is_some() { "write" } else { "create" },
        "lines": lines,
        "bytes": new_bytes.len(),
        "added": changed.as_ref().map(|diff| diff.added),
        "removed": changed.as_ref().map(|diff| diff.removed),
        "diff": diff_text,
        "diff_omitted": omitted,
    })))
}

/// `1 line`, `12 lines`.
pub(crate) fn lines_word(lines: usize) -> String {
    if lines == 1 {
        "1 line".to_owned()
    } else {
        format!("{} lines", count(lines))
    }
}
