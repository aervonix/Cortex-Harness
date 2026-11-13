//! The states of a turn, and which state may follow which
//! (`docs/architecture/03-agent-loop.md`, "Turn state machine").
//!
//! The loop is an explicit state machine: what it is doing is one value of
//! [`TurnState`], never a set of flags. Every change of state goes through
//! one function, which checks the change against [`TRANSITIONS`] in debug
//! builds and tells the session's [`Observer`], if it has one.

use cortex_protocol::SessionId;

/// What a session's loop is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TurnState {
    /// Nothing. No turn is running and no timer is set.
    Idle,
    /// Taking steering messages and notices, charging the ledger and
    /// assembling the next request.
    Preparing,
    /// Summarising, to start a new context epoch.
    Compacting,
    /// The request is being sent, and the response has not started.
    Requesting,
    /// The response is arriving.
    Streaming,
    /// An attempt failed and the client is waiting before the next one.
    Backoff,
    /// The response ended. The assistant item is recorded and the stop
    /// reason looked at.
    Deciding,
    /// The response's tool calls are being planned, approved, run and
    /// recorded.
    ToolRound,
    /// The project's checks are running.
    Verifying,
    /// The turn is over: the log is flushed and `turn.completed` sent.
    Finishing,
    /// The turn was cancelled: what exists is recorded.
    Interrupted,
}

impl TurnState {
    /// Every state, in the order of the page's table.
    pub const ALL: [Self; 11] = [
        Self::Idle,
        Self::Preparing,
        Self::Compacting,
        Self::Requesting,
        Self::Streaming,
        Self::Backoff,
        Self::Deciding,
        Self::ToolRound,
        Self::Verifying,
        Self::Finishing,
        Self::Interrupted,
    ];

    /// The state's name, in `snake_case`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Preparing => "preparing",
            Self::Compacting => "compacting",
            Self::Requesting => "requesting",
            Self::Streaming => "streaming",
            Self::Backoff => "backoff",
            Self::Deciding => "deciding",
            Self::ToolRound => "tool_round",
            Self::Verifying => "verifying",
            Self::Finishing => "finishing",
            Self::Interrupted => "interrupted",
        }
    }

    /// Whether the loop may go from this state to `next`.
    #[must_use]
    pub fn may_become(self, next: Self) -> bool {
        TRANSITIONS.contains(&(self, next))
    }
}

impl std::fmt::Display for TurnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

use TurnState::{
    Backoff, Compacting, Deciding, Finishing, Idle, Interrupted, Preparing, Requesting, Streaming,
    ToolRound, Verifying,
};

/// Every change of state the loop makes. Each one has a test
/// (`tests/it/transitions.rs`), and a test there fails when this table and
/// the tests disagree.
pub const TRANSITIONS: &[(TurnState, TurnState)] = &[
    // A prompt arrived.
    (Idle, Preparing),
    // The request is ready.
    (Preparing, Requesting),
    // The context is over its budget.
    (Preparing, Compacting),
    // A limit was reached, or the log could not be written.
    (Preparing, Finishing),
    (Preparing, Interrupted),
    // A new epoch began.
    (Compacting, Preparing),
    // The context cannot be made to fit.
    (Compacting, Finishing),
    (Compacting, Interrupted),
    // The response started.
    (Requesting, Streaming),
    // The attempt failed before it started, and another follows a wait.
    (Requesting, Backoff),
    // The provider said the request does not fit: compact, once.
    (Requesting, Compacting),
    // The request failed for good.
    (Requesting, Finishing),
    (Requesting, Interrupted),
    // The response ended.
    (Streaming, Deciding),
    // The attempt failed part-way, and another follows a wait.
    (Streaming, Backoff),
    // The attempt was given up and the request sent again at standard
    // speed.
    (Streaming, Requesting),
    // The provider said, part-way, that the request does not fit.
    (Streaming, Compacting),
    // The request failed for good.
    (Streaming, Finishing),
    (Streaming, Interrupted),
    // The wait is over and the request was sent again.
    (Backoff, Requesting),
    (Backoff, Interrupted),
    // The response has tool calls.
    (Deciding, ToolRound),
    // The model is done and the turn changed files.
    (Deciding, Verifying),
    // The model is done, or stopped in a way that ends the turn.
    (Deciding, Finishing),
    // The response needs a continuation.
    (Deciding, Preparing),
    // The response's stop reason says the request did not fit.
    (Deciding, Compacting),
    // Every result is recorded.
    (ToolRound, Preparing),
    // The same round keeps repeating, or the log could not be written.
    (ToolRound, Finishing),
    (ToolRound, Interrupted),
    // The checks passed, were skipped, or failed for the last time.
    (Verifying, Finishing),
    // The checks failed and the model is told.
    (Verifying, Preparing),
    (Verifying, Interrupted),
    // Nothing follows by itself.
    (Finishing, Idle),
    // The turn completed and a queued prompt starts the next one.
    (Finishing, Preparing),
    (Interrupted, Idle),
];

/// Something that is told of every change of state: a test, or later a
/// trace.
///
/// It is called on the session's own task, between two states, and must not
/// block.
pub trait Observer: Send + Sync {
    /// The session's loop went from `from` to `to`.
    fn transition(&self, session: &SessionId, from: TurnState, to: TurnState);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_no_duplicates_and_no_self_loops() {
        for (index, edge) in TRANSITIONS.iter().enumerate() {
            assert_ne!(edge.0, edge.1, "{edge:?}");
            assert!(!TRANSITIONS[..index].contains(edge), "{edge:?} twice");
        }
    }

    #[test]
    fn every_state_is_reached_and_left() {
        for state in TurnState::ALL {
            assert!(
                TRANSITIONS.iter().any(|(_, to)| *to == state),
                "{state} is never entered"
            );
            assert!(
                TRANSITIONS.iter().any(|(from, _)| *from == state),
                "{state} is never left"
            );
        }
    }

    #[test]
    fn a_turn_ends_only_through_finishing_or_interrupted() {
        let into_idle: Vec<TurnState> = TRANSITIONS
            .iter()
            .filter(|(_, to)| *to == Idle)
            .map(|(from, _)| *from)
            .collect();
        assert_eq!(into_idle, [Finishing, Interrupted]);
        assert!(Idle.may_become(Preparing));
        assert!(!Idle.may_become(Requesting));
        assert!(
            !Finishing.may_become(Interrupted),
            "a flush is not cut short"
        );
        assert_eq!(ToolRound.to_string(), "tool_round");
    }
}
