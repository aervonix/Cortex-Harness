//! What the loop asks of the permission engine
//! (`docs/architecture/06-permissions-and-sandbox.md`).
//!
//! The engine itself is `cortex-policy`, and the core connects it to the
//! front ends that answer questions. The loop sees both as one [`Policy`]:
//! it shows a planned call, and is told to run it, to refuse it, or to ask.
//! Asking is the policy's too, so that the loop never knows who answers, how
//! long they may take, or what an answer adds to the rules.
//!
//! [`AllowAll`] is the stand-in until the engine exists.

use std::sync::Arc;

use cortex_protocol::{CallId, SessionId, TurnId};
use cortex_tools::{BoxFuture, Json, OpenGate, Plan, ReadGate, ToolSpec};

/// One planned tool call, as the policy is shown it.
#[derive(Debug, Clone, Copy)]
pub struct PlannedCall<'a> {
    /// The session the call belongs to.
    pub session: &'a SessionId,
    /// The turn.
    pub turn: &'a TurnId,
    /// The provider's id for the call.
    pub call_id: &'a CallId,
    /// The tool's spec: its name, and what it does by default.
    pub spec: &'a ToolSpec,
    /// The arguments, as the model wrote them.
    pub arguments: &'a Json,
    /// What the call would do.
    pub plan: &'a Plan,
}

/// What the policy says of a planned call, before anybody is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// The call may run.
    Allow,
    /// Whoever supervises the session decides: the loop calls
    /// [`Policy::ask`].
    Ask,
    /// The call may not run.
    Deny {
        /// Why, for the model.
        reason: Option<String>,
    },
}

/// The answer to a question.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// The call may run.
    Allow,
    /// The call may not run.
    Deny {
        /// What the person who refused wrote, for the model.
        note: Option<String>,
    },
}

/// Decides whether a planned call may run.
pub trait Policy: Send + Sync {
    /// Decides a call from its plan, without asking anybody. It is called
    /// off the session's task, once per call, as soon as the call is
    /// planned, which can be while the model is still writing its response.
    fn decide(&self, call: &PlannedCall<'_>) -> Decision;

    /// Puts the question to whoever supervises the session, and returns the
    /// answer. Whatever an answer changes (a rule for the session, a stored
    /// approval) is the policy's to record.
    ///
    /// The loop asks one question at a time, in the model's call order. It
    /// drops the future when the turn is interrupted or the response the
    /// call came from is thrown away: the question is then withdrawn.
    fn ask<'a>(&'a self, call: PlannedCall<'a>) -> BoxFuture<'a, Verdict>;

    /// What a directory walk may read, entry by entry.
    fn read_gate(&self) -> Arc<dyn ReadGate> {
        Arc::new(OpenGate)
    }
}

/// The policy that allows every call: the stand-in until `cortex-policy`
/// exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

impl Policy for AllowAll {
    fn decide(&self, _call: &PlannedCall<'_>) -> Decision {
        Decision::Allow
    }

    fn ask<'a>(&'a self, _call: PlannedCall<'a>) -> BoxFuture<'a, Verdict> {
        Box::pin(std::future::ready(Verdict::Allow))
    }
}
