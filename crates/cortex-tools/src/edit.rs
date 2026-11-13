//! The `edit` tool (`docs/architecture/05-tools.md`, "`edit`").
//!
//! It replaces pieces of text in a file the model has read. Every `old` is
//! matched against the file as it was before the call, by the two tiers of
//! [`crate::matching`]; the pieces must not overlap; and either every edit
//! applies or the file is not touched. What the call does not replace does
//! not change by a byte: line endings and a byte-order mark stay as they
//! are, and new text gets the line ending of the lines it replaces.
//!
//! The steps of the change itself, and what each protects, are in
//! [`crate::change`].

use std::fmt::Write as _;

use cortex_base::ResolvedPath;
use serde::Deserialize;
use serde_json::json;

use crate::args;
use crate::change::{Env, Found, Target, payload_diff};
use crate::checkpoint::Editing;
use crate::context::{PlanCx, RunCx};
use crate::contract::{BoxFuture, ConcurrencyClass, Json, Plan, Tool, ToolOutput, ToolSpec};
use crate::diff;
use crate::matching::{self, Edit, Failure, Replacement};
use crate::output::count;
use crate::result::{ToolError, ToolResult};
use crate::textfile::{BOM, Text};
use crate::write::{plan_change, run_change};

/// The most edits one call may hold.
const MAX_EDITS: usize = 100;

/// The largest `count` an edit may give. It bounds how many replacements
/// one call can make, with [`MAX_EDITS`].
const MAX_COUNT: usize = 10_000;

/// How many places a result lists by line before it says "and more".
const MAX_PLACES_LISTED: usize = 20;

/// The `edit` tool.
#[derive(Debug)]
pub struct EditTool {
    spec: ToolSpec,
    editing: Editing,
}

impl EditTool {
    /// The tool, recording its changes and taking its waits as `editing`
    /// says.
    #[must_use]
    pub fn new(editing: Editing) -> Self {
        Self {
            spec: ToolSpec::new(
                "edit",
                "Replace pieces of text in a file. Each edit gives `old`, the text to replace, \
                 copied exactly from the file as `read` shows it, and `new`, what to put in \
                 its place. `old` must occur exactly `count` times (1 unless given), and every \
                 occurrence is replaced: add lines around it until it names one place. All \
                 edits are matched against the file as it is before the call, they must not \
                 overlap, and either all apply or none does. The file must have been read in \
                 this session and not have changed since. If `old` is not found exactly, a \
                 match that ignores trailing white space and a uniform difference in \
                 indentation is tried, and the result says so. On failure the result shows \
                 the closest text and how it differs.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "minLength": 1,
                            "description": "The file: relative to the workspace, absolute, or starting with ~."
                        },
                        "edits": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": MAX_EDITS,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old": {
                                        "type": "string",
                                        "minLength": 1,
                                        "description": "The text to replace, exactly as it is in the file."
                                    },
                                    "new": {
                                        "type": "string",
                                        "description": "The text to put in its place. Empty to remove `old`."
                                    },
                                    "count": {
                                        "type": "integer",
                                        "minimum": 1,
                                        "maximum": MAX_COUNT,
                                        "description": "How many times `old` must occur. Default 1."
                                    }
                                },
                                "required": ["old", "new"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["path", "edits"],
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
    edits: Vec<EditArgument>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgument {
    old: String,
    new: String,
    count: Option<usize>,
}

/// A validated call.
#[derive(Debug, Clone)]
struct Call {
    path: ResolvedPath,
    edits: Vec<Edit>,
}

impl Tool for EditTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        let arguments: Arguments = args::parse(&self.spec, args)?;
        let path = cx.paths.resolve(&arguments.path)?;
        let total = arguments.edits.len();
        let mut edits = Vec::with_capacity(total);
        for (index, edit) in arguments.edits.into_iter().enumerate() {
            if edit.old == edit.new {
                return Err(ToolError::invalid(format!(
                    "{}: `old` and `new` are the same text, so there is nothing to change.",
                    capitalised(&name(index, total))
                )));
            }
            edits.push(Edit {
                old: edit.old,
                new: edit.new,
                count: edit.count.unwrap_or(1),
            });
        }
        plan_change(
            format!(
                "Edit {} ({})",
                path.display(),
                if total == 1 {
                    "1 edit".to_owned()
                } else {
                    format!("{total} edits")
                }
            ),
            &path,
            Call {
                path: path.clone(),
                edits,
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
            run_change(cx, "edit", move || {
                edit_file(&call, &env).unwrap_or_else(|refusal| refusal)
            })
            .await
        })
    }
}

fn edit_file(call: &Call, env: &Env) -> Result<ToolResult, ToolResult> {
    let target = Target::new(&call.path, &env.paths)?;
    let shown = &target.shown;
    let found = target.find(env)?;
    let Found::File(existing) = &found else {
        return Err(ToolResult::error(format!(
            "`{shown}` does not exist, so there is nothing to edit. Use `write` to create it."
        )));
    };
    let (marked, old_text) = target.text(existing)?;
    target.guard(env, existing, "edit")?;

    let text = Text::new(old_text);
    let stop = || env.cancel.is_cancelled();
    let places = matching::locate(&text, &call.edits, &stop).map_err(|failures| {
        if failures.contains(&Failure::Stopped) {
            ToolResult::interrupted(format!(
                "`edit` was interrupted while it looked for the text in `{shown}`. Nothing was \
                 changed."
            ))
        } else {
            ToolResult::error(explain(shown, call.edits.len(), &failures))
        }
    })?;
    let body = matching::apply(old_text, &places);
    if body == old_text {
        return Ok(ToolResult::success(format!(
            "`{shown}` already has this text: with the edits made it would be exactly as it \
             is. Nothing was written."
        ))
        .with_payload(json!({"path": shown, "kind": "unchanged"})));
    }
    let mut new = String::with_capacity(body.len() + BOM.len());
    if marked {
        new.push_str(BOM);
    }
    new.push_str(&body);
    target.commit(env, &found, new.as_bytes())?;

    let written = matching::written_lines(old_text, &places);
    let changed = diff::diff_bounded(old_text, &body);
    let (diff_text, omitted) = payload_diff(changed.as_ref());
    let mut message = format!("Edited `{shown}`: {}.", places_list(&written));
    let tolerant = report_tolerant(&mut message, call.edits.len(), &places);
    Ok(ToolResult::success(message).with_payload(json!({
        "path": shown,
        "kind": "edit",
        "lines": Text::new(&body).lines.len(),
        "replacements": places.len(),
        "ranges": written
            .iter()
            .map(|&(first, last)| json!([first, last.unwrap_or(first)]))
            .collect::<Vec<_>>(),
        "tolerant": tolerant,
        "added": changed.as_ref().map(|diff| diff.added),
        "removed": changed.as_ref().map(|diff| diff.removed),
        "diff": diff_text,
        "diff_omitted": omitted,
    })))
}

/// Says, for every place the tolerant tier found, that it was used and what
/// text was replaced. Whether there was any.
fn report_tolerant(message: &mut String, total: usize, places: &[Replacement]) -> bool {
    let mut any = false;
    for place in places {
        let Some(tolerant) = &place.tolerant else {
            continue;
        };
        any = true;
        // Writing to a string cannot fail.
        let _ = write!(
            message,
            "\n{} did not match exactly. A match that ignores trailing white space and a \
             uniform difference in indentation was used, at {}. The text replaced was:\n{}",
            capitalised(&name(place.edit, total)),
            span(place.lines),
            tolerant.replaced
        );
        if !tolerant.reindented.is_empty() {
            let _ = write!(
                message,
                "\nThe new text was written with {} each of its lines, to match.",
                tolerant.reindented
            );
        }
    }
    any
}

/// Why nothing was changed, with what the model needs to put it right in
/// one more call.
fn explain(shown: &str, total: usize, failures: &[Failure]) -> String {
    let mut text = format!("`edit` changed nothing in `{shown}`.");
    for failure in failures {
        text.push('\n');
        match failure {
            Failure::NotFound { edit, closest } => {
                let _ = write!(
                    text,
                    "{}: `old` was not found.",
                    capitalised(&name(*edit, total))
                );
                match closest {
                    Some(closest) if !closest.diff.is_empty() => {
                        let _ = write!(
                            text,
                            " The closest text is at {}. Lines marked `-` are in your `old`, \
                             and lines marked `+` are what the file has there:\n{}",
                            span(closest.lines),
                            closest.diff
                        );
                    }
                    Some(closest) => {
                        let _ = write!(
                            text,
                            " The closest text is at {}, whose lines read the same as `old`'s: \
                             check where `old` begins and ends, and its white space.",
                            span(closest.lines)
                        );
                    }
                    None => text.push_str(
                        " Nothing in the file is close to it. Read the file again and copy \
                         the text to replace from it.",
                    ),
                }
                if let Some(note) = closest.as_ref().and_then(|closest| closest.note.as_ref()) {
                    text.push('\n');
                    text.push_str(note);
                }
            }
            Failure::WrongCount {
                edit,
                wanted,
                at,
                more,
                tolerant,
            } => {
                let found = if *more {
                    format!("more than {} times", count(at.len()))
                } else {
                    times(at.len())
                };
                let how = if *tolerant {
                    "`old` is not in the file exactly. With trailing white space and \
                     indentation ignored it matches"
                } else {
                    "`old` occurs"
                };
                let _ = write!(
                    text,
                    "{}: {how} {found} (at {}), and `count` is {wanted}. ",
                    capitalised(&name(*edit, total)),
                    lines_list(at, *more),
                );
                if *more || at.len() > *wanted {
                    text.push_str("Add lines around it to `old` until it names the place you mean");
                    if !*more {
                        let _ = write!(
                            text,
                            ", or set `count` to {} to replace every one",
                            at.len()
                        );
                    }
                    text.push('.');
                } else {
                    let _ = write!(text, "Set `count` to {}.", at.len());
                }
            }
            Failure::SelfOverlap { edit, at } => {
                let _ = write!(
                    text,
                    "{}: the places where `old` occurs share text (at {}), so replacing each \
                     of them has no one meaning. Make `old` longer.",
                    capitalised(&name(*edit, total)),
                    lines_list(at, false),
                );
            }
            Failure::Overlap {
                first,
                second,
                first_lines,
                second_lines,
            } => {
                let _ = write!(
                    text,
                    "Edits {} and {} would replace the same text ({} and {}). Make them one \
                     edit.",
                    first + 1,
                    second + 1,
                    span(*first_lines),
                    span(*second_lines),
                );
            }
            // Never among other failures: it is returned alone, and the
            // caller answers it before it gets here.
            Failure::Stopped => text.push_str("The call was interrupted."),
            Failure::CannotReindent { edit, lines, extra } => {
                let _ = write!(
                    text,
                    "{}: `old` matches {} only when indentation is ignored: its lines have \
                     {extra} more than the file's. `new` could not be given the same change, \
                     because one of its lines has less indentation than that. Write `old` and \
                     `new` with the file's own indentation.",
                    capitalised(&name(*edit, total)),
                    span(*lines),
                );
            }
        }
    }
    text
}

/// What an edit is called in a message: `the edit` in a call of one, `edit
/// 2` otherwise.
fn name(index: usize, total: usize) -> String {
    if total == 1 {
        "the edit".to_owned()
    } else {
        format!("edit {}", index + 1)
    }
}

fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// `once`, `twice`, `3 times`.
fn times(number: usize) -> String {
    match number {
        1 => "once".to_owned(),
        2 => "twice".to_owned(),
        number => format!("{} times", count(number)),
    }
}

/// `line 4`, `lines 4-6`.
fn span((first, last): (usize, usize)) -> String {
    if first == last {
        format!("line {first}")
    } else {
        format!("lines {first}-{last}")
    }
}

/// `line 4`, `lines 4 and 9`, `lines 4, 9 and 30`; with `more`, the list is
/// the first of them.
fn lines_list(lines: &[usize], more: bool) -> String {
    let shown: Vec<String> = lines
        .iter()
        .take(MAX_PLACES_LISTED)
        .map(usize::to_string)
        .collect();
    let word = if lines.len() == 1 { "line" } else { "lines" };
    if more || lines.len() > shown.len() {
        format!("the first at {word} {}", shown.join(", "))
    } else {
        format!("{word} {}", joined(&shown))
    }
}

/// Where a call wrote, as the result says it: `line 3`, `lines 10-14 and
/// line 30`, and for text that was taken out, the line it was taken from.
fn places_list(written: &[(usize, Option<usize>)]) -> String {
    let mut shown: Vec<String> = written
        .iter()
        .take(MAX_PLACES_LISTED)
        .map(|&(first, last)| match last {
            Some(last) => span((first, last)),
            None => format!("text removed at line {first}"),
        })
        .collect();
    if written.len() > shown.len() {
        shown.push(format!("{} more places", written.len() - shown.len()));
    }
    joined(&shown)
}

/// `a`, `a and b`, `a, b and c`.
fn joined(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [most @ .., last] => format!("{} and {last}", most.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_and_counts_read_as_a_sentence() {
        assert_eq!(span((4, 4)), "line 4");
        assert_eq!(span((4, 6)), "lines 4-6");
        assert_eq!(lines_list(&[4], false), "line 4");
        assert_eq!(lines_list(&[4, 9], false), "lines 4 and 9");
        assert_eq!(lines_list(&[4, 9, 30], false), "lines 4, 9 and 30");
        assert_eq!(lines_list(&[4, 9], true), "the first at lines 4, 9");
        assert_eq!(
            places_list(&[(10, Some(14)), (30, Some(30)), (41, None)]),
            "lines 10-14, line 30 and text removed at line 41"
        );
        let many: Vec<(usize, Option<usize>)> = (1..=25).map(|line| (line, Some(line))).collect();
        assert!(places_list(&many).ends_with("line 20 and 5 more places"));
        assert_eq!(times(1), "once");
        assert_eq!(times(2), "twice");
        assert_eq!(times(7), "7 times");
        assert_eq!(name(0, 1), "the edit");
        assert_eq!(name(1, 3), "edit 2");
        assert_eq!(capitalised("the edit"), "The edit");
    }
}
