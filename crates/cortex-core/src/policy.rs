//! The permission engine, as the loop, the tools and the session store
//! reach it (`docs/architecture/06-permissions-and-sandbox.md`).
//!
//! `cortex-policy`'s engine decides and records; it asks nobody and can see
//! none of the crates that need its answers. This module adapts it to the
//! three seams:
//!
//! | Seam | Here |
//! |---|---|
//! | The loop's `Policy::decide` | The plan's effects as the engine's accesses, and its outcome as the loop's decision. |
//! | The loop's `Policy::ask` | The question put to the front ends that may answer, the wait and its time limit, and the answer recorded. |
//! | The tools' `ReadGate` | `Engine::may_read`. |
//! | The base state's `KeepGate` | `Engine::may_store`. |
//!
//! # When nobody can be asked
//!
//! A call that needs an answer, in a session with no front end that can
//! give one, is refused: the model is told why, and the round goes on. The
//! refusal is kept ([`PermissionDenial`]), because it is the usual reason a
//! run without a person does less than was asked of it. With
//! `fail_on_denial` the same call ends the turn, through the loop's `Stop`,
//! with the outcome `denied`.

#![allow(
    clippy::disallowed_types,
    reason = "recording an approval for the project writes a file, on the blocking pool"
)]

use std::future::poll_fn;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::Poll;
use std::time::Duration;

use cortex_agent::{Decision, PlannedCall, Policy, Verdict};
use cortex_base::{AbsPath, SharedTimer};
use cortex_policy::{Access, Engine, Outcome, Question, Request};
use cortex_protocol::event::{Event, RequestResolved, Resolution};
use cortex_protocol::jsonrpc::RequestId;
use cortex_protocol::lease::Effect as EffectKind;
use cortex_protocol::methods::{
    ApprovalChoice, ApprovalDetail, ApprovalRequest, ApprovalResponse, CommandDetail,
    ServerRequest, TargetDetail, ToolDetail,
};
use cortex_protocol::{CallId, TurnId};
use cortex_session::KeepGate;
use cortex_tools::{BoxFuture, Effect, NetworkTarget, ReadGate};

use crate::clients::Clients;
use crate::events::SessionSink;
use crate::store::Hooks;

/// A tool call that was refused because nobody could be asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDenial {
    /// The turn the call belonged to.
    pub turn_id: TurnId,
    /// The call.
    pub call_id: CallId,
    /// The tool.
    pub tool: String,
    /// The call in one line.
    pub summary: String,
    /// Why it needed an answer.
    pub reason: String,
}

/// One session's engine, adapted.
pub(crate) struct EnginePolicy {
    engine: RwLock<Arc<Engine>>,
    clients: Arc<Clients>,
    sink: Arc<SessionSink>,
    hooks: Arc<Hooks>,
    timer: SharedTimer,
    /// How long an approval may wait for its answer. `None`: as long as it
    /// takes.
    timeout: Option<Duration>,
    fail_on_denial: bool,
    /// Whether commands run confined: what an approval's detail says.
    sandboxed: bool,
    denials: Mutex<Vec<PermissionDenial>>,
}

impl std::fmt::Debug for EnginePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnginePolicy")
            .field("fail_on_denial", &self.fail_on_denial)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// What a policy is made from.
pub(crate) struct PolicyParts {
    pub(crate) engine: Engine,
    pub(crate) clients: Arc<Clients>,
    pub(crate) sink: Arc<SessionSink>,
    pub(crate) hooks: Arc<Hooks>,
    pub(crate) timer: SharedTimer,
    pub(crate) timeout: Option<Duration>,
    pub(crate) fail_on_denial: bool,
    pub(crate) sandboxed: bool,
}

impl EnginePolicy {
    pub(crate) fn new(parts: PolicyParts) -> Self {
        Self {
            engine: RwLock::new(Arc::new(parts.engine)),
            clients: parts.clients,
            sink: parts.sink,
            hooks: parts.hooks,
            timer: parts.timer,
            timeout: parts.timeout,
            fail_on_denial: parts.fail_on_denial,
            sandboxed: parts.sandboxed,
            denials: Mutex::new(Vec::new()),
        }
    }

    /// The engine in force.
    pub(crate) fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Puts another engine in force: the session's level changed.
    pub(crate) fn replace(&self, engine: Engine) {
        *self.engine.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(engine);
    }

    /// The calls refused so far because nobody could be asked.
    pub(crate) fn denials(&self) -> Vec<PermissionDenial> {
        self.denials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// What the base state may keep: what the read rules would let the
    /// agent read unasked.
    pub(crate) fn keep_gate(&self) -> Arc<dyn KeepGate> {
        let engine = self.engine();
        Arc::new(move |path: &AbsPath| engine.may_store(path))
    }

    fn outcome(&self, call: &PlannedCall<'_>) -> Outcome {
        let accesses = accesses(call);
        self.engine().decide(&Request {
            tool: &call.spec.name,
            default: call.spec.permission,
            accesses: &accesses,
        })
    }

    /// The answer when nobody can be asked.
    fn nobody(&self, call: &PlannedCall<'_>, question: &Question) -> Verdict {
        let reason = question.refusal();
        self.denials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(PermissionDenial {
                turn_id: call.turn.clone(),
                call_id: call.call_id.clone(),
                tool: call.spec.name.clone(),
                summary: call.plan.summary.clone(),
                reason: question.reasons.join("; "),
            });
        if self.fail_on_denial {
            Verdict::Stop { reason }
        } else {
            Verdict::Deny { note: Some(reason) }
        }
    }

    fn resolved(&self, id: &RequestId, resolution: Resolution) {
        self.hooks.emit(Event::RequestResolved(RequestResolved {
            request_id: id.clone(),
            resolution,
        }));
    }
}

/// The plan's effects, as the engine reads them.
fn accesses(call: &PlannedCall<'_>) -> Vec<Access> {
    call.plan
        .effects
        .iter()
        .filter_map(|effect| {
            Some(match effect {
                Effect::Read(path) => Access::Read {
                    path: path.abs().clone(),
                },
                Effect::ReadTree(path) => Access::ReadTree {
                    path: path.abs().clone(),
                },
                Effect::Write(path) => Access::Write {
                    path: path.abs().clone(),
                },
                Effect::Exec { command, cwd } => Access::Exec {
                    command: command.clone(),
                    cwd: cwd.abs().clone(),
                },
                Effect::Network(NetworkTarget::Host(host)) => Access::Network {
                    host: Some(host.clone()),
                },
                Effect::Network(NetworkTarget::Any) => Access::Network { host: None },
                Effect::Ext { tool } => Access::Ext { tool: tool.clone() },
                // An effect this version does not know cannot be shown to
                // the engine as something it knows. The loop refuses a plan
                // it cannot place; nothing here widens it.
                _ => return None,
            })
        })
        .collect()
}

/// Exactly what an approval would let happen, for the front end to show.
fn detail(call: &PlannedCall<'_>, sandboxed: bool) -> ApprovalDetail {
    let effects = &call.plan.effects;
    let command = effects.iter().find_map(|effect| match effect {
        Effect::Exec { command, cwd } => Some(ApprovalDetail::Command(CommandDetail {
            command: command.clone(),
            cwd: cwd.abs().to_native_string(),
            outside_sandbox: !sandboxed,
        })),
        _ => None,
    });
    if let Some(command) = command {
        return command;
    }
    let target = |effect: EffectKind, target: String| {
        ApprovalDetail::Target(TargetDetail { effect, target })
    };
    match effects.first() {
        Some(Effect::Write(path)) => target(EffectKind::Write, path.abs().to_native_string()),
        Some(Effect::Read(path) | Effect::ReadTree(path)) => {
            target(EffectKind::Read, path.abs().to_native_string())
        }
        Some(Effect::Network(host)) => target(
            EffectKind::Network,
            match host {
                NetworkTarget::Host(host) => host.clone(),
                _ => "any host".to_owned(),
            },
        ),
        _ => ApprovalDetail::Tool(ToolDetail {
            arguments: call.arguments.clone(),
        }),
    }
}

/// An open request, which is closed and reported as withdrawn if the wait
/// for its answer is given up: the turn was interrupted, or the response
/// the call came from was thrown away.
struct Asked<'a> {
    policy: &'a EnginePolicy,
    id: RequestId,
    settled: bool,
}

impl Drop for Asked<'_> {
    fn drop(&mut self) {
        if !self.settled && self.policy.clients.close(&self.id) {
            self.policy.resolved(&self.id, Resolution::Cancelled);
        }
    }
}

impl Policy for EnginePolicy {
    fn decide(&self, call: &PlannedCall<'_>) -> Decision {
        match self.outcome(call) {
            Outcome::Allow { read_only: true } => Decision::AllowReadOnly,
            Outcome::Allow { read_only: false } => Decision::Allow,
            Outcome::Ask(_) => Decision::Ask,
            Outcome::Deny { reason } => Decision::Deny {
                reason: Some(reason),
            },
        }
    }

    fn ask<'a>(&'a self, call: PlannedCall<'a>) -> BoxFuture<'a, Verdict> {
        Box::pin(async move {
            // Decided again: an answer to an earlier question of the round
            // may have settled this one.
            let question = match self.outcome(&call) {
                Outcome::Allow { .. } => return Verdict::Allow,
                Outcome::Deny { reason } => return Verdict::Deny { note: Some(reason) },
                Outcome::Ask(question) => question,
            };
            let asked = self.clients.approvers(&self.sink.subscribers());
            if asked.is_empty() {
                return self.nobody(&call, &question);
            }

            let request = ApprovalRequest {
                session_id: call.session.clone(),
                turn_id: call.turn.clone(),
                call_id: call.call_id.clone(),
                tool: call.spec.name.clone(),
                summary: call.plan.summary.clone(),
                detail: detail(&call, self.sandboxed),
                choices: question.choices(),
                reasons: question.reasons.clone(),
                adds: question.lifts.clone(),
            };
            let (id, mut answer) = self.clients.ask(call.session, &asked, |_| {
                ServerRequest::Approval(request.clone())
            });
            let mut open = Asked {
                policy: self,
                id: id.clone(),
                settled: false,
            };
            let mut limit = self.timeout.map(|limit| self.timer.sleep(limit));
            let answered: Option<ApprovalResponse> = poll_fn(|cx| {
                if let Poll::Ready(response) = std::pin::Pin::new(&mut answer).poll(cx) {
                    return Poll::Ready(response.ok());
                }
                if let Some(limit) = &mut limit
                    && limit.as_mut().poll(cx).is_ready()
                {
                    return Poll::Ready(None);
                }
                Poll::Pending
            })
            .await;
            open.settled = true;

            let Some(response) = answered else {
                // Nobody answered in time: a denial, as the protocol says.
                self.clients.close(&id);
                self.resolved(&id, Resolution::TimedOut);
                return Verdict::Deny {
                    note: Some(format!(
                        "Nobody answered the approval request in time. {}",
                        question.refusal()
                    )),
                };
            };
            // The clients that did not answer take their prompt down.
            self.resolved(&id, Resolution::Answered);

            let engine = self.engine();
            let choice = response.choice.clone();
            let recorded = tokio::task::spawn_blocking(move || engine.record(&question, &choice));
            let allowed = match recorded.await {
                Ok(Ok(recorded)) => recorded.allowed,
                // The call was approved; what could not be done is keeping
                // the approval for later.
                Ok(Err(error)) => {
                    self.hooks.tell(format!(
                        "The approval was given, and could not be stored for later: {error}"
                    ));
                    !matches!(
                        response.choice,
                        ApprovalChoice::Deny | ApprovalChoice::Unknown(_)
                    )
                }
                Err(_) => false,
            };
            if allowed {
                Verdict::Allow
            } else {
                Verdict::Deny {
                    note: response.note,
                }
            }
        })
    }

    fn read_gate(&self) -> Arc<dyn ReadGate> {
        Arc::new(EngineGate(self.engine()))
    }
}

/// What a directory walk may read, entry by entry: the engine's read rules.
struct EngineGate(Arc<Engine>);

impl ReadGate for EngineGate {
    fn may_read(&self, path: &AbsPath) -> bool {
        self.0.may_read(path)
    }
}
