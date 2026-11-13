//! The exit codes of headless mode: one table for every output format
//! (`docs/architecture/10-headless.md`, "Exit codes").

use cortex_protocol::ErrorKind;
use cortex_protocol::event::TurnOutcome;

/// The turn completed and the model ended normally.
pub(crate) const COMPLETED: u8 = 0;
/// Internal error. A bug.
pub(crate) const INTERNAL: u8 = 1;
/// Usage or configuration error. Nothing was sent to a model.
pub(crate) const USAGE: u8 = 2;
/// No credentials, or the provider rejected them.
pub(crate) const AUTH: u8 = 3;
/// Provider or network error, after retries.
pub(crate) const PROVIDER: u8 = 4;
/// The model stopped without finishing.
pub(crate) const MODEL_STOP: u8 = 5;
/// A required permission was denied.
pub(crate) const DENIED: u8 = 6;
/// A configured limit was reached.
pub(crate) const LIMIT: u8 = 7;
/// The context could not be made to fit.
pub(crate) const CONTEXT: u8 = 8;
/// Interrupted: SIGINT, or Ctrl-C.
pub(crate) const INTERRUPTED: u8 = 130;
/// Terminated: SIGTERM, or Ctrl-Break or a close event on Windows.
pub(crate) const TERMINATED: u8 = 143;

/// Which signal stopped a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// SIGINT, or Ctrl-C.
    Interrupt,
    /// SIGTERM, or Ctrl-Break or a close event.
    Terminate,
}

impl Signal {
    /// The exit code a run stopped by this signal ends with.
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Interrupt => INTERRUPTED,
            Self::Terminate => TERMINATED,
        }
    }
}

/// The exit code of a failure of this kind.
pub(crate) fn for_kind(kind: &ErrorKind) -> u8 {
    match kind {
        ErrorKind::Usage | ErrorKind::Config => USAGE,
        ErrorKind::Auth => AUTH,
        ErrorKind::Provider | ErrorKind::Transient => PROVIDER,
        ErrorKind::ModelStop => MODEL_STOP,
        ErrorKind::Permission => DENIED,
        ErrorKind::Limit => LIMIT,
        ErrorKind::ContextOverflow => CONTEXT,
        ErrorKind::Interrupted => INTERRUPTED,
        // A tool's failure goes to the model and does not end a turn: one
        // that does is not expected, and is reported as the harness's own.
        ErrorKind::Tool | ErrorKind::Internal => INTERNAL,
        // An open enumeration: a kind a later version adds is a failure of
        // a sort this one cannot name.
        _ => INTERNAL,
    }
}

/// The outcome a failure of this kind has, when it ends a run before or
/// outside a turn: the outcome and the exit code always agree.
pub(crate) fn outcome_for_kind(kind: &ErrorKind) -> TurnOutcome {
    match kind {
        ErrorKind::Permission => TurnOutcome::Denied,
        ErrorKind::Limit => TurnOutcome::Limit,
        ErrorKind::Interrupted => TurnOutcome::Interrupted,
        _ => TurnOutcome::Failed,
    }
}

/// The exit code of a run that ended with `outcome`, the failure `kind`
/// when there was one, after `signal` if a signal stopped it.
pub(crate) fn for_outcome(
    outcome: &TurnOutcome,
    kind: Option<&ErrorKind>,
    signal: Option<Signal>,
) -> u8 {
    match outcome {
        TurnOutcome::Completed => COMPLETED,
        TurnOutcome::Denied => DENIED,
        TurnOutcome::Limit => LIMIT,
        TurnOutcome::Interrupted => signal.map_or(INTERRUPTED, Signal::code),
        TurnOutcome::Failed => kind.map_or(INTERNAL, for_kind),
        _ => INTERNAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_failure_has_the_code_of_the_table() {
        let table = [
            (ErrorKind::Usage, 2),
            (ErrorKind::Config, 2),
            (ErrorKind::Auth, 3),
            (ErrorKind::Provider, 4),
            (ErrorKind::Transient, 4),
            (ErrorKind::ModelStop, 5),
            (ErrorKind::Permission, 6),
            (ErrorKind::Limit, 7),
            (ErrorKind::ContextOverflow, 8),
            (ErrorKind::Interrupted, 130),
            (ErrorKind::Internal, 1),
            (ErrorKind::Tool, 1),
            (ErrorKind::from_wire("a_later_kind"), 1),
        ];
        for (kind, code) in table {
            assert_eq!(for_kind(&kind), code, "{kind:?}");
            assert_eq!(
                for_outcome(&TurnOutcome::Failed, Some(&kind), None),
                code,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn each_outcome_has_the_code_of_the_table() {
        assert_eq!(for_outcome(&TurnOutcome::Completed, None, None), 0);
        assert_eq!(for_outcome(&TurnOutcome::Denied, None, None), 6);
        assert_eq!(for_outcome(&TurnOutcome::Limit, None, None), 7);
        assert_eq!(for_outcome(&TurnOutcome::Failed, None, None), 1);
        assert_eq!(for_outcome(&TurnOutcome::Interrupted, None, None), 130);
        assert_eq!(
            for_outcome(&TurnOutcome::Interrupted, None, Some(Signal::Interrupt)),
            130
        );
        assert_eq!(
            for_outcome(&TurnOutcome::Interrupted, None, Some(Signal::Terminate)),
            143
        );
        assert_eq!(
            for_outcome(&TurnOutcome::from_wire("a_later_outcome"), None, None),
            1
        );
    }

    #[test]
    fn a_failure_outside_a_turn_has_an_outcome_that_agrees_with_its_code() {
        for kind in [
            ErrorKind::Usage,
            ErrorKind::Auth,
            ErrorKind::Permission,
            ErrorKind::Limit,
            ErrorKind::Interrupted,
            ErrorKind::Internal,
        ] {
            let outcome = outcome_for_kind(&kind);
            assert_eq!(
                for_outcome(&outcome, Some(&kind), None),
                for_kind(&kind),
                "{kind:?}"
            );
        }
    }
}
