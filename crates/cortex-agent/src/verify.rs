//! The verification stage, behind a trait
//! (`docs/architecture/15-efficiency.md`, "Automatic verification").
//!
//! When the model ends a turn that changed files, the loop enters
//! `Verifying` and asks a [`Verifier`] to run the project's checks. A
//! failure is appended as a notice and the model carries on, up to a limit
//! of rounds. Choosing the checks, running them and shaping their output is
//! the verifier's; the build step for verification supplies the real one.
//! Until then [`NoVerifier`] skips.

use cortex_base::{AbsPath, CancelToken};
use cortex_protocol::{SessionId, TurnId};
use cortex_tools::BoxFuture;

/// What one round of verification is given.
#[derive(Debug, Clone)]
pub struct VerifyCx<'a> {
    /// The session.
    pub session: &'a SessionId,
    /// The turn.
    pub turn: &'a TurnId,
    /// Which round of this turn it is, counted from 1.
    pub round: u32,
    /// The files the turn's tools have written so far, each a real
    /// location, sorted.
    pub written: &'a [AbsPath],
    /// Fires when the checks must stop.
    pub cancel: CancelToken,
}

/// What a round of verification found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VerifyOutcome {
    /// The checks passed.
    Passed,
    /// Nothing ran: no check is known, the mode is off, or the checks were
    /// stopped.
    Skipped,
    /// A check failed.
    Failed {
        /// What failed, for the model: bounded, with the lines that matter.
        report: String,
    },
}

/// Runs the project's checks.
pub trait Verifier: Send + Sync {
    /// One round. It stops promptly when `cx.cancel` fires.
    fn verify<'a>(&'a self, cx: VerifyCx<'a>) -> BoxFuture<'a, VerifyOutcome>;
}

/// The verifier of a session with no checks: every round is skipped.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoVerifier;

impl Verifier for NoVerifier {
    fn verify<'a>(&'a self, _cx: VerifyCx<'a>) -> BoxFuture<'a, VerifyOutcome> {
        Box::pin(std::future::ready(VerifyOutcome::Skipped))
    }
}
