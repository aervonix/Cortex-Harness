//! The repetition guard (`docs/architecture/03-agent-loop.md`, "Loop guards
//! and limits").
//!
//! A model can get stuck issuing the same calls and getting the same
//! results. The guard looks at each tool round as a whole: the calls, their
//! arguments and what came back. When a round is the same as the one before
//! it a few times in a row, the model is told. When it goes on, the turn
//! stops and the user decides.
//!
//! A response whose tool call could not be read counts as a round too, so
//! a model that keeps writing the same broken call is stopped the same way.

/// When the guard speaks and when it stops the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepetitionLimits {
    /// After this many identical rounds in a row the model is told that it
    /// is repeating itself.
    pub warn_at: u32,
    /// After this many the turn stops.
    pub stop_at: u32,
}

impl Default for RepetitionLimits {
    fn default() -> Self {
        Self {
            warn_at: 3,
            stop_at: 5,
        }
    }
}

/// What the guard makes of a round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repetition {
    /// Nothing to say.
    Fine,
    /// Tell the model: this is the `n`th identical round.
    Warn(u32),
    /// Stop the turn: this is the `n`th identical round.
    Stop(u32),
}

/// Counts identical rounds in a row.
#[derive(Debug, Clone)]
pub(crate) struct RepetitionGuard {
    limits: Option<RepetitionLimits>,
    last: Option<String>,
    count: u32,
}

impl RepetitionGuard {
    /// A guard with these limits, or one that never speaks.
    pub(crate) fn new(limits: Option<RepetitionLimits>) -> Self {
        Self {
            limits,
            last: None,
            count: 0,
        }
    }

    /// Forgets what came before: a new turn starts from nothing.
    pub(crate) fn reset(&mut self) {
        self.last = None;
        self.count = 0;
    }

    /// Takes the signature of the round that just ended: everything that
    /// would have to be equal for the round to have had no effect.
    pub(crate) fn observe(&mut self, signature: String) -> Repetition {
        let Some(limits) = self.limits else {
            return Repetition::Fine;
        };
        if self.last.as_deref() == Some(signature.as_str()) {
            self.count += 1;
        } else {
            self.last = Some(signature);
            self.count = 1;
        }
        if self.count >= limits.stop_at {
            Repetition::Stop(self.count)
        } else if self.count == limits.warn_at {
            Repetition::Warn(self.count)
        } else {
            Repetition::Fine
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_warns_once_at_three_and_stops_at_five() {
        let mut guard = RepetitionGuard::new(Some(RepetitionLimits::default()));
        let seen: Vec<Repetition> = (0..5).map(|_| guard.observe("same".to_owned())).collect();
        assert_eq!(
            seen,
            [
                Repetition::Fine,
                Repetition::Fine,
                Repetition::Warn(3),
                Repetition::Fine,
                Repetition::Stop(5)
            ]
        );
    }

    #[test]
    fn a_different_round_starts_the_count_again() {
        let mut guard = RepetitionGuard::new(Some(RepetitionLimits {
            warn_at: 2,
            stop_at: 3,
        }));
        assert_eq!(guard.observe("a".to_owned()), Repetition::Fine);
        assert_eq!(guard.observe("a".to_owned()), Repetition::Warn(2));
        assert_eq!(guard.observe("b".to_owned()), Repetition::Fine);
        assert_eq!(guard.observe("a".to_owned()), Repetition::Fine);
        assert_eq!(guard.observe("a".to_owned()), Repetition::Warn(2));
        assert_eq!(guard.observe("a".to_owned()), Repetition::Stop(3));
        guard.reset();
        assert_eq!(guard.observe("a".to_owned()), Repetition::Fine);
    }

    #[test]
    fn a_guard_that_is_off_never_speaks() {
        let mut guard = RepetitionGuard::new(None);
        for _ in 0..20 {
            assert_eq!(guard.observe("same".to_owned()), Repetition::Fine);
        }
    }
}
