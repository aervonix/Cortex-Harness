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

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_base::{CancelToken, IdCounter};
use cortex_llm::SpeedCooldowns;
use cortex_protocol::error::code;
use cortex_protocol::event::{Event, SessionUpdated};
use cortex_protocol::item::{Delivery, NoticeSource};
use cortex_protocol::lease::Level;
use cortex_protocol::methods::{Disposition, TurnInterruptResult, TurnStartResult};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::SessionState;
use cortex_protocol::usage::SessionUsage;
use cortex_protocol::{
    ErrorData, ErrorKind, LeaseId, ModelId, Part, ProviderId, SessionId, Speed, TurnId,
};
use tokio::sync::Notify;

use crate::config::{Models, RoleModel, SessionInfo};
use crate::events::Emitter;
use crate::speed::{SpeedSettings, role_state};
use crate::state::TurnState;

fn history_changing() -> Box<ErrorData> {
    Box::new(ErrorData::new(
        ErrorKind::Usage,
        code::HISTORY_CHANGING,
        "The session's history is being changed (a rewind). Send this again when that is done.",
    ))
}

/// A change to a session's history that is under way
/// ([`SessionHandle::change_history`]). No turn starts while it lives, so
/// its owner keeps it for as long as the change's work goes on, wherever
/// that work runs. Dropping it ends the change, and the loop reads the
/// history again before its next turn.
#[must_use = "the change ends when this is dropped"]
pub struct HistoryChange {
    shared: Arc<Shared>,
    made: bool,
}

impl HistoryChange {
    /// Says that the history did change. The session's table of seen files
    /// then forgets every file when the loop reads the history again: what
    /// it recorded may be of turns the history no longer has, and the model
    /// is to read a file before it changes it. A change that is given up
    /// before it changes anything leaves the table as it was, but for a
    /// new epoch.
    pub fn made(&mut self) {
        self.made = true;
    }
}

impl std::fmt::Debug for HistoryChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryChange")
            .field("session", &self.shared.info.id)
            .field("made", &self.made)
            .finish()
    }
}

impl Drop for HistoryChange {
    fn drop(&mut self) {
        let mut inbox = self.shared.lock();
        inbox.changing = false;
        inbox.reload = true;
        inbox.history_changed |= self.made;
    }
}

/// The turn that is running, or about to.
#[derive(Debug, Clone)]
pub(crate) struct Running {
    pub(crate) id: TurnId,
    /// The turn's token: a child of the session's.
    pub(crate) cancel: CancelToken,
    /// The turn's second token, "kill now": fired by a second interrupt,
    /// and handed to every tool call of the turn
    /// (`docs/architecture/02-runtime.md`, "Cancellation").
    pub(crate) kill: CancelToken,
}

/// A prompt that starts a turn.
#[derive(Debug, Clone)]
pub(crate) struct Prompt {
    pub(crate) input: Vec<Part>,
    /// How the prompt was sent, when it was sent during a running turn.
    pub(crate) delivery: Option<Delivery>,
    /// A turn that only compacts the conversation, with what the summary
    /// should keep in particular: `session/compact` on an idle session.
    pub(crate) compact: Option<Option<String>>,
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
    /// A compaction the user asked for, with what the summary should keep
    /// in particular.
    Compact(Option<String>),
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
    /// The models, by role. A change applies from the next request.
    pub(crate) models: Models,
    /// The level of the lease in force, and the lease.
    pub(crate) level: Level,
    pub(crate) lease_id: Option<LeaseId>,
    /// Whether the sandbox confines the session's commands: it follows the
    /// level, since nothing is confined at `unrestricted`.
    pub(crate) sandbox: cortex_protocol::session::SandboxStatus,
    /// The session's title.
    pub(crate) title: Option<String>,
    pub(crate) state: TurnState,
    pub(crate) usage: SessionUsage,
    /// The session's items changed under the loop (a rewind): it reads them
    /// again before its next turn.
    pub(crate) reload: bool,
    /// The session's history is being changed ([`HistoryChange`]): no turn
    /// starts, and no second change begins.
    pub(crate) changing: bool,
    /// A change of history was made ([`HistoryChange::made`]): when the
    /// loop reads the history again, the table of seen files forgets every
    /// file.
    pub(crate) history_changed: bool,
    /// Memory may hold what the log does not (an append failed, or the
    /// loop panicked): the history read again is not the one the last
    /// request was built from.
    pub(crate) diverged: bool,
    /// Whether the conversation has anything in it, from this process or
    /// from the log it was opened from ([`SessionHandle::has_history`]).
    pub(crate) history: bool,
    /// Context windows that a local server reported for the models it has
    /// loaded, which stand in for the catalog's.
    pub(crate) windows: BTreeMap<(ProviderId, ModelId), u64>,
    pub(crate) closed: bool,
}

pub(crate) struct Shared {
    /// The session as it was opened. What can change afterwards (the
    /// level, the lease, the title) is in the inbox, which is what the
    /// state reports.
    pub(crate) info: SessionInfo,
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
            title: inbox.title.clone(),
            created_at: self.info.created_at.clone(),
            parent: self.info.parent.clone(),
            level: inbox.level.clone(),
            lease_id: inbox.lease_id.clone(),
            sandbox: inbox.sandbox.clone(),
            speed: inbox.speeds.session.clone(),
            roles: inbox
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
    /// `history_changing` while the session's history is being changed
    /// ([`change_history`](Self::change_history)): the prompt is sent again
    /// when that is over.
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
        // Looked at under the same lock as the change sets it: a prompt
        // either starts its turn before the change begins, and the change
        // is then refused, or is refused itself.
        if inbox.changing {
            return Err(history_changing());
        }
        let Some(running) = inbox.running.clone() else {
            let turn: TurnId = inbox.turn_ids.next_id();
            inbox.running = Some(Running {
                id: turn.clone(),
                cancel: shared.cancel.child(),
                kill: CancelToken::new(),
            });
            inbox.starting = Some(Prompt {
                input,
                delivery: None,
                compact: None,
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
                        compact: None,
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
            // An interrupt means stop: a compaction that was asked for
            // during the turn is dropped with it.
            Pending::Compact(_) => false,
            Pending::Notice { .. } => true,
        });
        drop(inbox);
        running.cancel.cancel();
        TurnInterruptResult {
            interrupted: true,
            undelivered,
        }
    }

    /// A second interrupt, "kill now": the running turn is interrupted as
    /// by [`interrupt`](Self::interrupt), and its tools are told to stop at
    /// once. A command that is being stopped in stages loses what is left
    /// of its graces, and one that had not been asked yet is killed
    /// without being asked (`docs/architecture/02-runtime.md`,
    /// "Cancellation").
    ///
    /// It can be the first interrupt a turn gets. With no turn running it
    /// does nothing.
    #[must_use]
    pub fn interrupt_now(&self) -> TurnInterruptResult {
        let kill = self
            .shared
            .lock()
            .running
            .as_ref()
            .map(|running| running.kill.clone());
        // The ordinary interrupt first, so that a tool that looks at the
        // second token finds the first one fired too.
        let result = self.interrupt();
        if let Some(kill) = kill {
            kill.cancel();
        }
        result
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

    /// Compacts the conversation now (`session/compact`): a new context
    /// epoch that begins with a summary of this one. `focus` says what the
    /// summary should keep in particular.
    ///
    /// - **The session is idle:** a turn starts that only compacts. It ends
    ///   with `turn.completed`, and sends the model nothing else.
    /// - **A turn is running:** the compaction is made at the turn's next
    ///   safe boundary, before its next request.
    ///
    /// # Errors
    ///
    /// `session_not_open` after [`close`](Self::close); `history_changing`
    /// while the history is being changed.
    pub fn compact(&self, focus: Option<String>) -> Result<TurnStartResult, Box<ErrorData>> {
        let shared = &self.shared;
        let mut inbox = shared.lock();
        if inbox.closed {
            return Err(Box::new(ErrorData::new(
                ErrorKind::Usage,
                code::SESSION_NOT_OPEN,
                "The session has been closed.",
            )));
        }
        if inbox.changing {
            return Err(history_changing());
        }
        if let Some(running) = inbox.running.clone() {
            inbox.pending.push_back(Pending::Compact(focus));
            return Ok(TurnStartResult {
                turn_id: running.id,
                disposition: Disposition::Steering,
            });
        }
        let turn: TurnId = inbox.turn_ids.next_id();
        inbox.running = Some(Running {
            id: turn.clone(),
            cancel: shared.cancel.child(),
            kill: CancelToken::new(),
        });
        inbox.starting = Some(Prompt {
            input: Vec::new(),
            delivery: None,
            compact: Some(focus),
        });
        drop(inbox);
        shared.wake.notify_one();
        Ok(TurnStartResult {
            turn_id: turn,
            disposition: Disposition::Started,
        })
    }

    /// Records the context window a local server loaded a model with: the
    /// session holds that model's requests to it in place of the catalog's
    /// figure (`docs/architecture/04-providers.md`, "A local server's
    /// loaded context size").
    pub fn set_context_window(&self, provider: ProviderId, model: ModelId, tokens: u64) {
        self.shared.lock().windows.insert((provider, model), tokens);
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

    /// Replaces the model of a role: another model, another provider's
    /// client, another reasoning level or output limit. It applies from
    /// the role's next request; a request that is being prepared or is in
    /// flight keeps the model it was made for.
    ///
    /// The conversation is not touched. What a new model is sent of the
    /// reasoning an earlier one wrote is the provider layer's rule
    /// (`docs/architecture/04-providers.md`, "The conversation model").
    pub fn set_role_model(&self, role: ModelRole, model: RoleModel) {
        let mut inbox = self.shared.lock();
        let models = inbox.models.clone().with_role(role, model);
        inbox.models = models;
        self.shared.emit_updated(&inbox);
    }

    /// The model a role runs on now. `None` for a role with no model.
    #[must_use]
    pub fn role_model(&self, role: &ModelRole) -> Option<RoleModel> {
        let inbox = self.shared.lock();
        inbox
            .models
            .roles()
            .find(|(known, _)| known == role)
            .map(|(_, model)| model.clone())
    }

    /// Records that the session's lease changed: its level, and the lease
    /// now in force. The loop holds no lease itself; this is what the
    /// session's state reports.
    pub fn set_level(&self, level: Level, lease_id: Option<LeaseId>) {
        let mut inbox = self.shared.lock();
        if inbox.level != level || inbox.lease_id != lease_id {
            inbox.level = level;
            inbox.lease_id = lease_id;
            self.shared.emit_updated(&inbox);
        }
    }

    /// Records whether the sandbox confines the session's commands from
    /// now on: what the session's state reports.
    pub fn set_sandbox(&self, sandbox: cortex_protocol::session::SandboxStatus) {
        let mut inbox = self.shared.lock();
        if inbox.sandbox != sandbox {
            inbox.sandbox = sandbox;
            self.shared.emit_updated(&inbox);
        }
    }

    /// Sets the session's title.
    pub fn set_title(&self, title: Option<String>) {
        let mut inbox = self.shared.lock();
        if inbox.title != title {
            inbox.title = title;
            self.shared.emit_updated(&inbox);
        }
    }

    /// Sends front ends an event of the session that the loop does not
    /// make itself: the plan changed, a notice from whoever assembled the
    /// session, a request that was resolved. It takes the next `seq`, and
    /// belongs to the running turn when there is one.
    ///
    /// **Not to be called from the session's [`EventSink`](crate::EventSink).**
    pub fn emit(&self, event: Event) {
        let inbox = self.shared.lock();
        let turn = inbox.running.as_ref().map(|running| &running.id);
        self.shared.emitter.emit(turn, event);
    }

    /// The session's state now: what is set, what is in effect, the turn
    /// that is running and how many prompts wait behind it.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.shared.state(&self.shared.lock())
    }

    /// Whether the session's conversation has anything in it: a resumed
    /// session's from its log, and whatever this process added. A change
    /// of model then sends it all again without the provider's cached
    /// prefix. ([`usage`](Self::usage) counts this process's requests only,
    /// and starts at none on resume.)
    #[must_use]
    pub fn has_history(&self) -> bool {
        self.shared.lock().history
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

    /// Begins a change to the session's items under the loop (a rewind, a
    /// change of branch), which its owner makes in the store while the
    /// returned [`HistoryChange`] lives. Meanwhile no turn starts
    /// ([`prompt`](Self::prompt) is refused with `history_changing`) and no
    /// second change begins. When it is dropped, however the change ended,
    /// the loop reads the session's items again before its next turn: if
    /// that fails, the turn fails, and the turn after it tries again.
    ///
    /// Reading them again starts a new context epoch of the session's table
    /// of seen files: the model no longer has what it was shown before, so
    /// the next read of any file is in full
    /// (`docs/architecture/07-sessions-and-context.md`, "What each part of
    /// a session's state follows").
    ///
    /// # Errors
    ///
    /// `turn_running` while a turn is running: history is not changed under
    /// a turn. `history_changing` while another change is under way.
    /// `session_not_open` after [`close`](Self::close).
    pub fn change_history(&self) -> Result<HistoryChange, Box<ErrorData>> {
        let mut inbox = self.shared.lock();
        if inbox.closed {
            return Err(Box::new(ErrorData::new(
                ErrorKind::Usage,
                code::SESSION_NOT_OPEN,
                "The session has been closed.",
            )));
        }
        if inbox.running.is_some() {
            return Err(Box::new(ErrorData::new(
                ErrorKind::Usage,
                code::TURN_RUNNING,
                "A turn is running. Interrupt it, or wait for it, before changing the session's \
                 history.",
            )));
        }
        if inbox.changing {
            return Err(history_changing());
        }
        inbox.changing = true;
        Ok(HistoryChange {
            shared: Arc::clone(&self.shared),
            made: false,
        })
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
