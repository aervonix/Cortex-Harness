//! The plan, and the `plan` tool
//! (`docs/architecture/05-tools.md`, "`plan`").
//!
//! The plan is a task list that the agent keeps and the user can see and
//! change while the agent works. Two parties write it:
//!
//! - **The model**, with the `plan` tool. Each call sends the whole list
//!   and replaces the plan.
//! - **The user**, from the plan panel or through the RPC protocol:
//!   reorder, skip, reword, add, remove ([`PlanBoard::apply`]). A change
//!   takes effect at once, and [`PlanChange::notice`] is the text that
//!   tells the model exactly what changed. The core hands it to the loop
//!   (`SessionHandle::notice` with `NoticeSource::Plan`), which delivers it
//!   at the next safe boundary.
//!
//! **The user's edit wins over a call that crosses it.** A `plan` call may
//! have been written before the model saw the user's edit, and its list
//! would silently undo that edit. So the board remembers what the user has
//! changed that the model has not been shown. While there is any, a call
//! replaces nothing: its result says what the user changed and gives the
//! current plan, and the model's next call goes through. It is the
//! staleness guard of the file tools, for the plan.
//!
//! The model is shown an edit in one of two ways.
//!
//! - **By a result of the tool,** once that result has reached it. A
//!   result reaches the model with the next request, after the round. So
//!   an edit that a call was told about counts as shown only when a call
//!   of a **later round** arrives ([`RunCx::round`]): a second call in the
//!   same response was written without the first one's result, and is
//!   refused like the first. A caller that gives no round number is never
//!   taken to be in a later round.
//! - **By the notice.** The board cannot see a notice being delivered, so
//!   the core tells it ([`PlanBoard::delivered`]) when the loop has
//!   recorded one. With the loop's round numbers, a core that never does
//!   loses nothing but a call: the model's first `plan` call after an edit
//!   is answered with the current plan and replaces nothing, even when the
//!   model had read the notice.
//!
//! A [`PlanBoard`] belongs to one session. It is shared by the tool, which
//! the model calls, and by the core, which applies the user's edits, tells
//! front ends (`plan.updated`), and carries the plan across compaction and
//! a resume.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_protocol::ids::PlanItemId;
use cortex_protocol::item::{Plan as SessionPlan, PlanItem, PlanStatus};
use cortex_protocol::project::PlanEdit;
use serde::Deserialize;
use serde_json::json;

use crate::args;
use crate::context::{PlanCx, RunCx};
use crate::contract::{BoxFuture, Json, Plan, Tool, ToolOutput, ToolSpec};
use crate::result::{ToolError, ToolResult};

/// The most items a plan holds.
pub const MAX_PLAN_ITEMS: usize = 100;

/// The longest text of one item, in characters.
pub const MAX_PLAN_TEXT: usize = 400;

/// The longest id of one item, in characters.
const MAX_ID: usize = 40;

/// A session's plan.
#[derive(Debug, Default)]
pub struct PlanBoard {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    plan: SessionPlan,
    /// What the user has changed that the model has not been shown.
    unseen: Vec<Unseen>,
}

/// One change of the user's that the model has not been shown.
#[derive(Debug, Clone)]
struct Unseen {
    /// The revision the change made.
    revision: u64,
    /// What it was, in one line.
    change: String,
    /// The round of the call whose result told the model of it. The model
    /// has that result once a later round begins.
    told_in: Option<u64>,
}

/// What a user's edits did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChange {
    /// The plan now.
    pub plan: SessionPlan,
    /// What to tell the model: exactly what changed, and the plan as it now
    /// is. `None` when the edits changed nothing (an item skipped that was
    /// skipped already), and there is nothing to tell.
    pub notice: Option<String>,
}

/// Why a user's edits were not applied. None of them was.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanEditError {
    /// An edit names an item the plan does not have.
    #[error("the plan has no item `{0}`")]
    UnknownItem(String),
    /// A new order does not name every item exactly once.
    #[error("a new order must name every item of the plan exactly once")]
    NotAnOrdering,
    /// An item's text is empty, too long, or more than one line.
    #[error("{0}")]
    BadText(String),
    /// The plan is full.
    #[error("a plan holds at most {MAX_PLAN_ITEMS} items")]
    TooManyItems,
    /// An edit of a kind this build does not know.
    #[error("this version does not know that kind of plan edit")]
    NotUnderstood,
}

/// What a `plan` call did.
enum Replaced {
    /// The list is the plan now. `changed` is false when it already was.
    Saved { plan: SessionPlan, changed: bool },
    /// The user had edited the plan since the model last saw it. Nothing
    /// was replaced.
    Crossed {
        plan: SessionPlan,
        edits: Vec<String>,
    },
}

impl PlanBoard {
    /// An empty plan.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A board that starts with a plan: the one a resumed session had.
    #[must_use]
    pub fn with_plan(plan: SessionPlan) -> Self {
        Self {
            state: Mutex::new(State {
                plan,
                unseen: Vec::new(),
            }),
        }
    }

    /// Puts the plan of another point of the session in place: the one a
    /// rewind went back to. What the user had changed and the model had
    /// not been shown is forgotten with the plan it was changed in. The
    /// revision never goes back, so that a front end can tell the plan it
    /// holds is older.
    pub fn restore(&self, mut plan: SessionPlan) {
        let mut state = self.lock();
        plan.revision = plan.revision.max(state.plan.revision + 1);
        state.plan = plan;
        state.unseen.clear();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every change is made on a copy and put in place in one step.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The plan now.
    #[must_use]
    pub fn get(&self) -> SessionPlan {
        self.lock().plan.clone()
    }

    /// Applies a user's edits, in order, all or none. The stored plan
    /// changes at once.
    ///
    /// # Errors
    ///
    /// When an edit does not fit the plan. The plan is then as it was.
    pub fn apply(&self, edits: &[PlanEdit]) -> Result<PlanChange, PlanEditError> {
        let mut state = self.lock();
        let mut items = state.plan.items.clone();
        let mut changes = Vec::new();
        for edit in edits {
            apply_one(&mut items, edit, &mut changes)?;
        }
        if changes.is_empty() {
            return Ok(PlanChange {
                plan: state.plan.clone(),
                notice: None,
            });
        }
        state.plan = SessionPlan {
            items,
            revision: state.plan.revision + 1,
        };
        let mut notice = String::from("The user edited the plan:");
        for change in &changes {
            let _ = write!(notice, "\n- {change}");
        }
        let _ = write!(
            notice,
            "\n{}\nWork from this plan. A `plan` call replaces the whole list, so base your next \
             one on it.",
            render(&state.plan)
        );
        let revision = state.plan.revision;
        state
            .unseen
            .extend(changes.into_iter().map(|change| Unseen {
                revision,
                change,
                told_in: None,
            }));
        Ok(PlanChange {
            plan: state.plan.clone(),
            notice: Some(notice),
        })
    }

    /// Says that the notice of a user's edit has reached the model: the one
    /// [`apply`](Self::apply) returned with the plan at `revision`. The
    /// core calls it when the loop has recorded that notice in the
    /// conversation, which is when the next request will carry it.
    ///
    /// Edits made after that revision stay unseen: their own notices are
    /// still on their way.
    pub fn delivered(&self, revision: u64) {
        self.lock()
            .unseen
            .retain(|unseen| unseen.revision > revision);
    }

    /// The model's call, in the tool round `round` (0 when the caller did
    /// not say): replaces the list, unless the user has edited the plan
    /// since the model last saw it.
    fn replace(&self, items: Vec<PlanItem>, round: u64) -> Replaced {
        let mut state = self.lock();
        // What a call of an earlier round was told, the model has been
        // shown: that round's results went out before this response was
        // written. A call of the same round was written beside the one that
        // was told, not after it. And with no round number there is no
        // telling, so nothing counts as shown.
        state.unseen.retain(|unseen| match unseen.told_in {
            Some(told) => round == 0 || told == 0 || told >= round,
            None => true,
        });
        // Whatever this call returns gives the model the current plan,
        // once its round is over.
        let unseen: Vec<String> = state
            .unseen
            .iter_mut()
            .map(|unseen| {
                unseen.told_in = Some(round);
                unseen.change.clone()
            })
            .collect();
        if items == state.plan.items {
            return Replaced::Saved {
                plan: state.plan.clone(),
                changed: false,
            };
        }
        if !unseen.is_empty() {
            return Replaced::Crossed {
                plan: state.plan.clone(),
                edits: unseen,
            };
        }
        state.plan = SessionPlan {
            items,
            revision: state.plan.revision + 1,
        };
        Replaced::Saved {
            plan: state.plan.clone(),
            changed: true,
        }
    }
}

fn position(items: &[PlanItem], id: &PlanItemId) -> Result<usize, PlanEditError> {
    items
        .iter()
        .position(|item| item.id == *id)
        .ok_or_else(|| PlanEditError::UnknownItem(id.as_str().to_owned()))
}

/// Makes one edit on `items`, and says what it changed, if anything.
fn apply_one(
    items: &mut Vec<PlanItem>,
    edit: &PlanEdit,
    changes: &mut Vec<String>,
) -> Result<(), PlanEditError> {
    match edit {
        PlanEdit::Reorder(reorder) => {
            let mut ordered = Vec::with_capacity(items.len());
            for id in &reorder.order {
                let item = &items[position(items, id).map_err(|_| PlanEditError::NotAnOrdering)?];
                if ordered.iter().any(|placed: &PlanItem| placed.id == item.id) {
                    return Err(PlanEditError::NotAnOrdering);
                }
                ordered.push(item.clone());
            }
            if ordered.len() != items.len() {
                return Err(PlanEditError::NotAnOrdering);
            }
            if ordered != *items {
                let order: Vec<String> = ordered
                    .iter()
                    .map(|item| format!("`{}`", item.id))
                    .collect();
                changes.push(format!("changed the order to {}", order.join(", ")));
                *items = ordered;
            }
        }
        PlanEdit::Skip(named) => {
            let at = position(items, &named.id)?;
            let item = &mut items[at];
            if item.status != PlanStatus::Skipped {
                item.status = PlanStatus::Skipped;
                changes.push(format!("skipped `{}` ({})", item.id, item.text));
            }
        }
        PlanEdit::Reword(reword) => {
            let text = checked_text(&reword.text).map_err(PlanEditError::BadText)?;
            let at = position(items, &reword.id)?;
            let item = &mut items[at];
            if item.text != text {
                changes.push(format!(
                    "reworded `{}`: it was \"{}\" and is now \"{text}\"",
                    item.id, item.text
                ));
                item.text = text;
            }
        }
        PlanEdit::Add(add) => {
            let text = checked_text(&add.text).map_err(PlanEditError::BadText)?;
            if items.len() >= MAX_PLAN_ITEMS {
                return Err(PlanEditError::TooManyItems);
            }
            let at = match &add.after {
                Some(after) => position(items, after)? + 1,
                None => items.len(),
            };
            // An id of the user's own, that no item has.
            let id = (1..)
                .map(|number| PlanItemId::new(format!("u{number}")))
                .find(|id| items.iter().all(|item| item.id != *id))
                .unwrap_or_else(|| PlanItemId::new("u"));
            changes.push(match &add.after {
                Some(after) => format!("added `{id}` after `{after}`: {text}"),
                None => format!("added `{id}` at the end: {text}"),
            });
            items.insert(
                at,
                PlanItem {
                    id,
                    text,
                    status: PlanStatus::Pending,
                },
            );
        }
        PlanEdit::Remove(named) => {
            let at = position(items, &named.id)?;
            let removed = items.remove(at);
            changes.push(format!("removed `{}` ({})", removed.id, removed.text));
        }
        _ => return Err(PlanEditError::NotUnderstood),
    }
    Ok(())
}

/// An item's text, trimmed, or what is wrong with it.
fn checked_text(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("an item's text is empty".to_owned());
    }
    if text.chars().any(char::is_control) {
        return Err("an item's text is one line, with no control characters".to_owned());
    }
    if text.chars().count() > MAX_PLAN_TEXT {
        return Err(format!(
            "an item's text is at most {MAX_PLAN_TEXT} characters"
        ));
    }
    Ok(text.to_owned())
}

/// The plan as the model is shown it.
fn render(plan: &SessionPlan) -> String {
    if plan.items.is_empty() {
        return format!("The plan is now empty (revision {}).", plan.revision);
    }
    let mut text = format!("The plan is now (revision {}):", plan.revision);
    for (number, item) in plan.items.iter().enumerate() {
        let _ = write!(
            text,
            "\n{}. [{}] {}: {}",
            number + 1,
            item.status.as_str(),
            item.id,
            item.text
        );
    }
    text
}

/// `4 items (1 done, 1 active, 2 pending)`.
fn summary(plan: &SessionPlan) -> String {
    if plan.items.is_empty() {
        return "no items".to_owned();
    }
    let mut counts: Vec<String> = Vec::new();
    for status in [
        PlanStatus::Done,
        PlanStatus::Active,
        PlanStatus::Pending,
        PlanStatus::Skipped,
    ] {
        let count = plan
            .items
            .iter()
            .filter(|item| item.status == status)
            .count();
        if count > 0 {
            counts.push(format!("{count} {}", status.as_str()));
        }
    }
    let items = if plan.items.len() == 1 {
        "1 item".to_owned()
    } else {
        format!("{} items", plan.items.len())
    };
    format!("{items} ({})", counts.join(", "))
}

/// The `plan` tool.
#[derive(Debug)]
pub struct PlanTool {
    spec: ToolSpec,
    board: Arc<PlanBoard>,
}

impl PlanTool {
    /// The tool, for the session whose plan `board` is.
    #[must_use]
    pub fn new(board: Arc<PlanBoard>) -> Self {
        Self {
            spec: ToolSpec::new(
                "plan",
                "Keep the plan for the current work: a short task list the user can see and \
                 edit while you work. Each call sends the full list and replaces the plan. Give \
                 each item a short `id` that stays the same from call to call, its `text` in \
                 one line, and its `status`. Mark an item `active` when you start it and \
                 `done` when it is finished. The user may reorder, reword, skip, add or remove \
                 items; you are told when they do, and if they did since you last saw the \
                 plan, your call replaces nothing and returns the current plan to work from.",
                json!({
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "maxItems": MAX_PLAN_ITEMS,
                            "description": "The full list, in order.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {
                                        "type": "string",
                                        "minLength": 1,
                                        "maxLength": MAX_ID,
                                        "description": "A short name for the item, the same in every call."
                                    },
                                    "text": {
                                        "type": "string",
                                        "minLength": 1,
                                        "description": "The task, in one line."
                                    },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "active", "done", "skipped"]
                                    }
                                },
                                "required": ["id", "text", "status"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["items"],
                    "additionalProperties": false
                }),
            ),
            board,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    items: Vec<ItemArgument>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemArgument {
    id: String,
    text: String,
    status: String,
}

impl Tool for PlanTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn plan(&self, args: &Json, _cx: &PlanCx) -> Result<Plan, ToolError> {
        let arguments: Arguments = args::parse(&self.spec, args)?;
        let mut items: Vec<PlanItem> = Vec::with_capacity(arguments.items.len());
        for (index, item) in arguments.items.into_iter().enumerate() {
            let number = index + 1;
            let id = item.id.trim();
            if id.is_empty()
                || id.chars().count() > MAX_ID
                || id
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace() || c == '`')
            {
                return Err(ToolError::invalid(format!(
                    "Item {number}: an `id` is a short name of up to {MAX_ID} characters with \
                     no spaces, such as `tests` or `step-2`."
                )));
            }
            if items.iter().any(|earlier| earlier.id.as_str() == id) {
                return Err(ToolError::invalid(format!(
                    "Item {number}: the id `{id}` is used twice. Every item needs an id of its \
                     own."
                )));
            }
            let text = checked_text(&item.text).map_err(|problem| {
                ToolError::invalid(format!("Item {number} (`{id}`): {problem}."))
            })?;
            items.push(PlanItem {
                id: PlanItemId::new(id),
                text,
                status: PlanStatus::from_wire(&item.status),
            });
        }
        let summary = match items.len() {
            0 => "Clear the plan".to_owned(),
            1 => "Update the plan (1 item)".to_owned(),
            count => format!("Update the plan ({count} items)"),
        };
        // Nothing outside the session is touched, so there is no effect to
        // declare. The marker is what keeps the call from running for a
        // response that is thrown away.
        Ok(Plan::new(summary, items).with_session_change())
    }

    fn run<'a>(
        &'a self,
        plan: Plan,
        cx: &'a RunCx,
        _out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let items = match plan.into_args::<Vec<PlanItem>>() {
                Ok(items) => items,
                Err(error) => return error.into_result(),
            };
            if cx.cancel.is_cancelled() {
                return ToolResult::interrupted(
                    "`plan` was interrupted before it started. The plan was not changed.",
                );
            }
            match self.board.replace(items, cx.round) {
                Replaced::Saved { plan, changed } => {
                    // What the model sent is the plan: no need to send it
                    // back.
                    let text = format!(
                        "Plan {}: {}, revision {}. It is exactly the list you sent.",
                        if changed { "saved" } else { "unchanged" },
                        summary(&plan),
                        plan.revision
                    );
                    ToolResult::success(text).with_payload(json!({
                        "plan": plan,
                        "replaced": changed,
                    }))
                }
                Replaced::Crossed { plan, edits } => {
                    let mut text = String::from(
                        "The plan was not replaced: the user has edited it since you last saw \
                         it, and your list was written without their changes. The user:",
                    );
                    for edit in &edits {
                        let _ = write!(text, "\n- {edit}");
                    }
                    let _ = write!(
                        text,
                        "\n{}\nSend your update again, based on this plan.",
                        render(&plan)
                    );
                    ToolResult::error(text).with_payload(json!({
                        "plan": plan,
                        "replaced": false,
                        "user_edits": edits,
                    }))
                }
            }
        })
    }
}
