//! The `remember` tool (`docs/architecture/05-tools.md`, "`remember`";
//! `docs/architecture/14-adaptation.md`, "Project knowledge").
//!
//! The model proposes a fact or a convention for the project's memory,
//! with where it learned it. A proposal is stored as proposed and is never
//! in the prompt: it becomes active only when the user accepts it in the
//! review panel.
//!
//! The project's memory is `cortex-knowledge`, which is built later and
//! which this crate cannot see. So the tool hands its proposals to a
//! [`Proposals`] trait, which that crate implements, and which
//! [`MemoryProposals`] implements for a session with nowhere to keep them.

use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::CancelToken;
use cortex_protocol::project::{EntryCheck, EntryCheckKind, EntryKind};
use serde::Deserialize;
use serde_json::json;

use crate::args;
use crate::blocking::run_blocking;
use crate::context::{PlanCx, RunCx};
use crate::contract::{BoxFuture, Json, Plan, Tool, ToolOutput, ToolSpec};
use crate::result::{ToolError, ToolResult};

/// The longest `text` of a proposal, in characters: one or two sentences.
pub const MAX_PROPOSAL_TEXT: usize = 600;

/// The longest `evidence`, and the longest pattern or command of a check,
/// in characters.
pub const MAX_PROPOSAL_DETAIL: usize = 400;

/// What the model proposes for the project's memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// A fact or a convention.
    pub kind: EntryKind,
    /// The entry, in a sentence or two.
    pub text: String,
    /// Where it was learned: a path and line, a command, or the user's
    /// words.
    pub evidence: String,
    /// A read-only test that shows the entry still holds. It is stored and
    /// shown with the proposal; nothing runs it yet. A path in it is
    /// relative to the workspace root.
    pub check: Option<EntryCheck>,
    /// The turn the proposal was made in, counted from 1. 0 when unknown.
    pub turn: u32,
}

/// What became of a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalOutcome {
    /// It is stored, waiting for the user's review.
    Stored,
    /// The same entry is already in the project's memory, or already
    /// waiting for review. Nothing was added.
    AlreadyKnown,
}

/// Why a proposal could not be stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProposalError(String);

impl ProposalError {
    /// An error with the store's own account of what went wrong.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Where proposals for the project's memory go.
///
/// It blocks, and is called on the blocking pool. A proposal it takes is
/// stored as proposed: nothing here may make an entry active.
pub trait Proposals: Send + Sync {
    /// Stores a proposal for the user's review.
    ///
    /// # Errors
    ///
    /// When the proposal cannot be stored.
    fn propose(&self, proposal: Proposal) -> Result<ProposalOutcome, ProposalError>;
}

/// Proposals kept in memory, for a session with no project store: tests,
/// and one-off runs that keep nothing.
#[derive(Debug, Clone, Default)]
pub struct MemoryProposals {
    proposals: Arc<Mutex<Vec<Proposal>>>,
}

impl MemoryProposals {
    /// No proposals yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every proposal stored, in order.
    #[must_use]
    pub fn proposals(&self) -> Vec<Proposal> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Proposal>> {
        // Every change is one push.
        self.proposals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Proposals for MemoryProposals {
    fn propose(&self, proposal: Proposal) -> Result<ProposalOutcome, ProposalError> {
        let mut proposals = self.lock();
        if proposals
            .iter()
            .any(|known| known.kind == proposal.kind && known.text == proposal.text)
        {
            return Ok(ProposalOutcome::AlreadyKnown);
        }
        proposals.push(proposal);
        Ok(ProposalOutcome::Stored)
    }
}

/// The `remember` tool.
pub struct RememberTool {
    spec: ToolSpec,
    proposals: Arc<dyn Proposals>,
}

impl RememberTool {
    /// The tool, storing its proposals in `proposals`.
    #[must_use]
    pub fn new(proposals: Arc<dyn Proposals>) -> Self {
        Self {
            spec: ToolSpec::new(
                "remember",
                "Propose a fact or a convention for the project's memory, so that later \
                 sessions in this project start with it: how the project is built or tested, \
                 a rule its code follows, something that was costly to find out. Give the \
                 entry in one or two sentences and say where you learned it. The entry is a \
                 proposal: it is used only after the user accepts it, so do not rely on it in \
                 this session. Do not propose what the project's instruction files already \
                 say, or anything that is only true of the current task.",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["fact", "convention"],
                            "description": "A fact about the project, or a rule its code and its contributors follow."
                        },
                        "text": {
                            "type": "string",
                            "minLength": 1,
                            "description": "The entry, in one or two sentences."
                        },
                        "evidence": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Where this was learned: a path and line, a command, or the user's words."
                        },
                        "check": {
                            "type": "object",
                            "description": "Optional: a read-only test that shows the entry still holds.",
                            "properties": {
                                "kind": {
                                    "type": "string",
                                    "enum": ["exists", "contains", "command"],
                                    "description": "`exists`: `path` exists. `contains`: a search of `path` finds `pattern`. `command`: `command` exits with success."
                                },
                                "path": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "For `exists` and `contains`: a path inside the workspace."
                                },
                                "pattern": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "For `contains`: a regular expression."
                                },
                                "command": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "For `command`: a command that only reads."
                                }
                            },
                            "required": ["kind"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["kind", "text", "evidence"],
                    "additionalProperties": false
                }),
            ),
            proposals,
        }
    }
}

impl std::fmt::Debug for RememberTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RememberTool").finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    kind: String,
    text: String,
    evidence: String,
    check: Option<CheckArgument>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckArgument {
    kind: String,
    path: Option<String>,
    pattern: Option<String>,
    command: Option<String>,
}

/// A text argument, trimmed and within its length, or what is wrong.
fn checked(name: &str, text: &str, longest: usize) -> Result<String, ToolError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ToolError::invalid(format!("`{name}` is empty.")));
    }
    let length = text.chars().count();
    if length > longest {
        return Err(ToolError::invalid(format!(
            "`{name}` is {length} characters long, and may be up to {longest}. Say it more \
             briefly."
        )));
    }
    Ok(text.to_owned())
}

fn check_of(argument: CheckArgument, cx: &PlanCx) -> Result<EntryCheck, ToolError> {
    let kind = EntryCheckKind::from_wire(&argument.kind);
    let (wanted, unwanted): (&[&str], &[&str]) = match kind {
        EntryCheckKind::Exists => (&["path"], &["pattern", "command"]),
        EntryCheckKind::Contains => (&["path", "pattern"], &["command"]),
        _ => (&["command"], &["path", "pattern"]),
    };
    let given = |name: &str| match name {
        "path" => argument.path.is_some(),
        "pattern" => argument.pattern.is_some(),
        _ => argument.command.is_some(),
    };
    if let Some(missing) = wanted.iter().find(|name| !given(name)) {
        return Err(ToolError::invalid(format!(
            "A check of kind `{}` needs `{missing}`.",
            argument.kind
        )));
    }
    if let Some(extra) = unwanted.iter().find(|name| given(name)) {
        return Err(ToolError::invalid(format!(
            "A check of kind `{}` takes {}, and not `{extra}`.",
            argument.kind,
            wanted
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        )));
    }
    // A check's path goes through the resolver like every other path, and
    // is kept relative to the workspace: a check reads the project, and
    // nothing else.
    let path = argument
        .path
        .map(|path| {
            let resolved = cx.paths.resolve(&path)?;
            match resolved.relative() {
                Some(relative) if !relative.is_empty() => Ok(relative.as_str().to_owned()),
                Some(_) => Ok(".".to_owned()),
                None => Err(ToolError::invalid(format!(
                    "The check's path `{}` is outside the workspace. A check reads the \
                     project's own files.",
                    resolved.display()
                ))),
            }
        })
        .transpose()?;
    Ok(EntryCheck {
        kind,
        path,
        pattern: argument
            .pattern
            .map(|pattern| checked("check.pattern", &pattern, MAX_PROPOSAL_DETAIL))
            .transpose()?,
        command: argument
            .command
            .map(|command| checked("check.command", &command, MAX_PROPOSAL_DETAIL))
            .transpose()?,
    })
}

impl Tool for RememberTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        let arguments: Arguments = args::parse(&self.spec, args)?;
        let text = checked("text", &arguments.text, MAX_PROPOSAL_TEXT)?;
        let proposal = Proposal {
            kind: EntryKind::from_wire(&arguments.kind),
            evidence: checked("evidence", &arguments.evidence, MAX_PROPOSAL_DETAIL)?,
            check: arguments
                .check
                .map(|check| check_of(check, cx))
                .transpose()?,
            text,
            turn: 0,
        };
        // Nothing outside the session and the project's own store of
        // proposals is touched: no effect to declare, and the marker keeps
        // the call from running for a response that is thrown away.
        Ok(Plan::new(
            format!("Propose a {} for the project's memory", arguments.kind),
            proposal,
        )
        .with_session_change())
    }

    fn run<'a>(
        &'a self,
        plan: Plan,
        cx: &'a RunCx,
        _out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let mut proposal = match plan.into_args::<Proposal>() {
                Ok(proposal) => proposal,
                Err(error) => return error.into_result(),
            };
            if cx.cancel.is_cancelled() {
                return ToolResult::interrupted(
                    "`remember` was interrupted before it started. Nothing was proposed.",
                );
            }
            proposal.turn = cx.turn;
            let kind = proposal.kind.as_str().to_owned();
            let text = proposal.text.clone();
            let has_check = proposal.check.is_some();
            let proposals = Arc::clone(&self.proposals);
            // Storing is not tied to the call's token: a proposal is stored
            // or it is not, and the result says which.
            let stored =
                run_blocking(&CancelToken::new(), move |_| proposals.propose(proposal)).await;
            let payload = |outcome: &str| json!({"kind": kind, "text": text, "outcome": outcome, "check": has_check});
            match stored {
                Ok(Ok(ProposalOutcome::Stored)) => ToolResult::success(format!(
                    "Proposed for the project's memory, as a {kind}. It is not in use: it \
                     waits for the user's review, and is used only if they accept it."
                ))
                .with_payload(payload("stored")),
                Ok(Ok(ProposalOutcome::AlreadyKnown)) => ToolResult::success(
                    "This is already in the project's memory, or already waiting for the \
                     user's review. Nothing was added.",
                )
                .with_payload(payload("already_known")),
                Ok(Err(error)) => ToolResult::error(format!(
                    "The proposal could not be stored: {error}. Nothing was added."
                )),
                Err(error) => args::unfinished(error, "remember"),
            }
        })
    }
}
