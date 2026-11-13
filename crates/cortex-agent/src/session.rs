//! A session's handle: how prompts, steering, interruption and settings
//! reach its loop (`docs/architecture/03-agent-loop.md`, "Steering and
//! queued prompts").
//!
//! The loop runs as one task and owns everything it works on. What other
//! code may do to a session it does through a [`SessionHandle`], which
//! shares one small piece of state with the loop, the session's inbox:
//! which turn is running, the messages waiting to be delivered, and the
//! settings a request reads when it is prepared.
//!
//! **Whether a turn is running is decided under the inbox's lock.** A
//! prompt that arrives while the session is idle starts a turn in that same
//! step, so two prompts can never both find the session idle, and the
//! answer a caller gets (started, steering, queued) is what happens.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_base::{CancelToken, IdCounter};
use cortex_llm::SpeedCooldowns;
use cortex_protocol::error::code;
use cortex_protocol::event::{Event, SessionUpdated};
use cortex_protocol::item::{Delivery, NoticeSource};
use cortex_protocol::methods::{Disposition, TurnInterruptResult, TurnStartResult};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::SessionState;
use cortex_protocol::usage::SessionUsage;
use cortex_protocol::{ErrorData, ErrorKind, Part, SessionId, Speed, TurnId};
use tokio::sync::Notify;

use crate::config::{Models, SessionInfo};
use crate::events::Emitter;
use crate::speed::{SpeedSettings, role_state};
use crate::state::TurnState;

/// The turn that is running, or about to.
#[derive(Debug, Clone)]
pub(crate) struct Running {
    pub(crate) id: TurnId,
    /// The turn's token: a child of the session's.
    pub(crate) cancel: CancelToken,
}

/// A prompt that starts a turn.
#[derive(Debug, Clone)]
pub(crate) struct Prompt {
    pub(crate) input: Vec<Part>,
    /// How the prompt was sent, when it was sent during a running turn.
    pub(crate) delivery: Option<Delivery>,
}

/// A prompt waiting to become a turn, with the id that turn will have.
#[derive(Debug, Clone)]
pub(crate) struct Queued {
    pub(crate) turn: TurnId,
    pub(crate) prompt: Prompt,
}

/// Something to deliver at the next safe boundary of the running turn.
#[derive(Debug, Clone)]
pub(crate) enum Pending {
    /// A steering message from the user.
    Steer(Vec<Part>),
    /// A notice for the model: an edit to the plan, changed instructions.
    Notice { source: NoticeSource, text: String },
}

/// What a handle and the loop share.
#[derive(Debug)]
pub(crate) struct Inbox {
    pub(crate) turn_ids: IdCounter,
    pub(crate) running: Option<Running>,
    /// The prompt of the running turn, until the loop takes it.
    pub(crate) starting: Option<Prompt>,
    /// Steering messages and notices, in the order they arrived.
    pub(crate) pending: VecDeque<Pending>,
    /// Prompts that follow the running turn, one at a time.
    pub(crate) queue: VecDeque<Queued>,
    /// Queued prompts that are held: the turn before them did not complete,
    /// so they wait for the user's next prompt and follow it.
    pub(crate) held: VecDeque<Queued>,
    pub(crate) speeds: SpeedSettings,
    pub(crate) state: TurnState,
    pub(crate) usage: SessionUsage,
    /// The session's items changed under the loop (a rewind): it reads them
    /// again before its next turn.
    pub(crate) reload: bool,
    pub(crate) closed: bool,
}

pub(crate) struct Shared {
    pub(crate) info: SessionInfo,
    pub(crate) models: Models,
    pub(crate) cooldowns: SpeedCooldowns,
    pub(crate) emitter: Arc<Emitter>,
    /// The session's token: a child of the process's.
    pub(crate) cancel: CancelToken,
    /// Wakes the loop when it is idle.
    pub(crate) wake: Notify,
    pub(crate) inbox: Mutex<Inbox>,
}

impl Shared {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Inbox> {
        // Each change to the inbox is complete before the lock is let go,
        // and none of them can panic part-way.
        self.inbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The session's state as it stands.
    pub(crate) fn state(&self, inbox: &Inbox) -> SessionState {
        SessionState {
            session_id: self.info.id.clone(),
            workspace: self.info.workspace.clone(),
            title: self.info.title.clone(),
            created_at: self.info.created_at.clone(),
            parent: self.info.parent.clone(),
            level: self.info.level.clone(),
            lease_id: self.info.lease_id.clone(),
            sandbox: self.info.sandbox.clone(),
            speed: inbox.speeds.session.clone(),
            roles: self
                .models
                .roles()
                .map(|(role, model)| {
                    let state = role_state(&role, model, &inbox.speeds, &self.cooldowns);
                    (role, state)
                })
                .collect(),
            seq: self.emitter.seq(),
            running_turn: inbox.running.as_ref().map(|running| running.id.clone()),
            queue_length: u32::try_from(inbox.queue.len() + inbox.held.len()).unwrap_or(u32::MAX),
        }
    }

    /// Tells front ends that the session's state changed. Called with the
    /// inbox locked, so that two changes are told in the order they were
    /// made.
    pub(crate) fn emit_updated(&self, inbox: &Inbox) {
        let turn = inbox.running.as_ref().map(|running| &running.id);
        self.emitter.emit(
            turn,
            Event::SessionUpdated(SessionUpdated {
                session: self.state(inbox),
            }),
        );
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("session", &self.info.id)
            .finish_non_exhaustive()
    }
}

/// A handle to a running session. Clones are the same session.
///
/// Every method returns at once and can be called from any thread.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    pub(crate) shared: Arc<Shared>,
}

impl SessionHandle {
    /// The session's id.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.shared.info.id
    }

    /// Sends a prompt.
    ///
    /// - **The session is idle:** the prompt starts a turn. `delivery` is
    ///   ignored. Prompts that were held behind a turn that did not
    ///   complete follow this one, in order.
    /// - **A turn is running:** `delivery` says what to do. `steer`
    ///   delivers the prompt at the next safe boundary of the running turn,
    ///   and if the turn completes before there is one, as the next turn.
    ///   `queue` makes it a turn of its own, after the running one
    ///   completes.
    ///
    /// The result means "accepted". The turn's end is the `turn.completed`
    /// event.
    ///
    /// # Errors
    ///
    /// `turn_running` when a turn is running and `delivery` is absent: the
    /// loop does not guess. `unknown_value` for a delivery this build does
    /// not know. `session_not_open` after [`close`](Self::close).
    pub fn prompt(
        &self,
        input: Vec<Part>,
        delivery: Option<Delivery>,
    ) -> Result<TurnStartResult, Box<ErrorData>> {
        let shared = &self.shared;
        let mut inbox = shared.lock();
        if inbox.closed {
            return Err(Box::new(ErrorData::new(
                ErrorKind::Usage,
                code::SESSION_NOT_OPEN,
                "The session has been closed.",
            )));
        }
        let Some(running) = inbox.running.clone() else {
            let turn: TurnId = inbox.turn_ids.next_id();
            inbox.running = Some(Running {
                id: turn.clone(),
                cancel: shared.cancel.child(),
            });
            inbox.starting = Some(Prompt {
                input,
                delivery: None,
            });
            // What was held follows this prompt, ahead of anything queued
            // from here on.
            let held = std::mem::take(&mut inbox.held);
            let later = std::mem::replace(&mut inbox.queue, held);
            inbox.queue.extend(later);
            drop(inbox);
            shared.wake.notify_one();
            return Ok(TurnStartResult {
                turn_id: turn,
                disposition: Disposition::Started,
            });
        };
        match delivery {
            Some(Delivery::Steer) => {
                inbox.pending.push_back(Pending::Steer(input));
                Ok(TurnStartResult {
                    turn_id: running.id,
                    disposition: Disposition::Steering,
                })
            }
            Some(Delivery::Queue) => {
                let turn: TurnId = inbox.turn_ids.next_id();
                inbox.queue.push_back(Queued {
                    turn: turn.clone(),
                    prompt: Prompt {
                        input,
                        delivery: Some(Delivery::Queue),
                    },
                });
                shared.emit_updated(&inbox);
                Ok(TurnStartResult {
                    turn_id: turn,
                    disposition: Disposition::Queued,
                })
            }
            None => Err(Box::new(ErrorData::new(
                ErrorKind::Usage,
                code::TURN_RUNNING,
                "A turn is running. Say whether to steer it or to queue the prompt behind it.",
            ))),
            Some(other) => Err(Box::new(ErrorData::new(
                ErrorKind::Usage,
                code::UNKNOWN_VALUE,
                format!(
                    "`{}` is not a way to deliver a prompt. Use `steer` or `queue`.",
                    other.as_str()
                ),
            ))),
        }
    }

    /// Interrupts the running turn. It ends with `turn.completed` and the
    /// outcome `interrupted`, with a history the next request accepts.
    ///
    /// Steering prompts that had not been delivered are handed back, for
    /// the front end's editor. Queued prompts stay queued, held until the
    /// user's next prompt. Calling it again, or with no turn running, does
    /// nothing.
    #[must_use]
    pub fn interrupt(&self) -> TurnInterruptResult {
        let mut inbox = self.shared.lock();
        let Some(running) = inbox.running.clone() else {
            return TurnInterruptResult::default();
        };
        let mut undelivered = Vec::new();
        inbox.pending.retain(|pending| match pending {
            Pending::Steer(input) => {
                undelivered.push(input.clone());
                false
            }
            Pending::Notice { .. } => true,
        });
        drop(inbox);
        running.cancel.cancel();
        TurnInterruptResult {
            interrupted: true,
            undelivered,
        }
    }

    /// Gives the model a notice at the next safe boundary: after the
    /// running turn's current tool round, or at the start of the next turn
    /// when none is running. This is how a user's edit to the plan reaches
    /// the model. A notice never starts a turn by itself.
    pub fn notice(&self, source: NoticeSource, text: impl Into<String>) {
        self.shared.lock().pending.push_back(Pending::Notice {
            source,
            text: text.into(),
        });
    }

    /// Sets the session's speed: what the `main` role runs at unless it has
    /// a speed of its own. It applies from the next request.
    pub fn set_speed(&self, speed: Speed) {
        let mut inbox = self.shared.lock();
        if inbox.speeds.session != speed {
            inbox.speeds.session = speed;
            self.shared.emit_updated(&inbox);
        }
    }

    /// Sets a role's own speed. It applies from the role's next request.
    pub fn set_role_speed(&self, role: ModelRole, speed: Speed) {
        let mut inbox = self.shared.lock();
        if inbox.speeds.roles.get(&role) != Some(&speed) {
            inbox.speeds.roles.insert(role, speed);
            self.shared.emit_updated(&inbox);
        }
    }

    /// The session's state now: what is set, what is in effect, the turn
    /// that is running and how many prompts wait behind it.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.shared.state(&self.shared.lock())
    }

    /// The session's usage, as of the last request that ended.
    #[must_use]
    pub fn usage(&self) -> SessionUsage {
        self.shared.lock().usage.clone()
    }

    /// What the session's loop is doing.
    #[must_use]
    pub fn turn_state(&self) -> TurnState {
        self.shared.lock().state
    }

    /// Tells the loop that the session's items changed under it (a rewind,
    /// a change of branch): it reads them again before its next turn. If
    /// that fails, the turn fails, and the turn after it tries again.
    ///
    /// # Errors
    ///
    /// `turn_running` while a turn is running: history is not changed under
    /// a turn.
    pub fn reload(&self) -> Result<(), Box<ErrorData>> {
        let mut inbox = self.shared.lock();
        if inbox.running.is_some() {
            return Err(Box::new(ErrorData::new(
                ErrorKind::Usage,
                code::TURN_RUNNING,
                "A turn is running. Interrupt it, or wait for it, before changing the session's \
                 history.",
            )));
        }
        inbox.reload = true;
        Ok(())
    }

    /// Closes the session: a running turn is interrupted, and the loop
    /// ends when it has recorded what exists. Prompts sent afterwards are
    /// refused.
    pub fn close(&self) {
        self.shared.lock().closed = true;
        self.shared.cancel.cancel();
        self.shared.wake.notify_one();
    }
}
