//! The retry layer's rules (`docs/architecture/03-agent-loop.md`, "Retries",
//! and `docs/architecture/04-providers.md`, "Speed tiers").
//!
//! This module decides and does nothing else: given a failure and what has
//! happened to the request so far, [`RetryPolicy::decide`] says whether to
//! wait and send again, to send at standard speed, or to give up. It does
//! not sleep, send or read a clock, so every rule is tested as a plain
//! function. The [client](crate::ProviderClient) carries the decisions out.
//!
//! # The rules
//!
//! - **Only `Transient` failures are retried.** A context overflow is for
//!   the agent loop to compact away; everything else is reported.
//! - **Backoff is exponential with full jitter:** a random wait between
//!   zero and a ceiling that starts at [`base`](RetryPolicy::base), grows by
//!   [`factor`](RetryPolicy::factor) with each failure and stops at
//!   [`cap`](RetryPolicy::cap).
//! - **The server's wait wins.** When the provider says how long to wait,
//!   that is the wait. When it is longer than
//!   [`max_wait`](RetryPolicy::max_wait) the request is not retried: the
//!   error is reported with the time, for a person or a script to decide.
//! - **A request gets [`max_attempts`](RetryPolicy::max_attempts) sends.**
//!
//! # A speed tier's own limit
//!
//! A failure can belong to the speed tier the request was sent at, not to
//! the model ([`ProviderError::tier`]). With
//! [`fallback`](SpeedPolicy::fallback) on, which is the default:
//!
//! - **A short wait is waited out in place.** While everything the request
//!   has waited for the tier stays within
//!   [`short_wait`](SpeedPolicy::short_wait), it waits as long as the
//!   provider asked and is sent again at the same speed. Nothing changes,
//!   and the cached prefix survives.
//! - **Otherwise the request falls back** to standard speed at once, and
//!   the tier goes into a cooldown ([`RetryPolicy::cooldown`]). This is the
//!   case when the wait is longer, when the provider named no wait, and
//!   when the attempts are used up. The refused attempt is not counted
//!   against the request: the fallback is not a retry of a failure.
//!
//! With fallback off, a tier's refusal is an ordinary rate limit, handled
//! by the rules above.

use std::time::Duration;

use cortex_base::{ErrorKind, Rng};
use cortex_protocol::Speed;
use cortex_protocol::speed::SpeedFallbackReason;

use crate::error::{ProviderError, Throttle};

/// How failed requests are retried, and the time limits on a response.
/// Every number is a setting (`docs/architecture/08-configuration.md`); the
/// defaults are the documented ones.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// The most times one request is sent. Default 5.
    pub max_attempts: u32,
    /// The ceiling of the first backoff wait. Default 1 s.
    pub base: Duration,
    /// What the ceiling is multiplied by after each failure. Default 2.
    pub factor: f64,
    /// The largest the ceiling gets. Default 30 s.
    pub cap: Duration,
    /// The longest wait a server may ask for and still be obeyed. A longer
    /// one ends the request with the error and the time. Default 2 minutes.
    pub max_wait: Duration,
    /// The longest silence on a response stream before the request fails as
    /// transient. Keep-alive pings count as bytes. A provider's entry may
    /// set its own. Default 90 s.
    pub idle_timeout: Duration,
    /// The longest wait, from sending a request, for the first event of its
    /// response. A provider's entry may set its own, and a local server
    /// that reads a long prompt before it answers needs to. Default 60 s.
    pub first_event_timeout: Duration,
    /// What to do when a speed tier's own limit refuses a request.
    pub speed: SpeedPolicy,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base: Duration::from_secs(1),
            factor: 2.0,
            cap: Duration::from_secs(30),
            max_wait: Duration::from_secs(120),
            idle_timeout: Duration::from_secs(90),
            first_event_timeout: Duration::from_secs(60),
            speed: SpeedPolicy::default(),
        }
    }
}

/// What to do when a speed tier's own limit or capacity refuses a request
/// (`docs/decisions/0027-speed-tiers.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedPolicy {
    /// Whether to send the request at standard speed when its tier is
    /// refused. Off makes the refusal an ordinary rate limit. Default on.
    pub fallback: bool,
    /// How long one request may wait in place for its tier, in total,
    /// before it falls back. Zero never waits in place. Default 20 s.
    pub short_wait: Duration,
    /// How long a cooldown lasts when the provider did not say when the
    /// tier is available again. Default 10 minutes.
    pub cooldown: Duration,
}

impl Default for SpeedPolicy {
    fn default() -> Self {
        Self {
            fallback: true,
            short_wait: Duration::from_secs(20),
            cooldown: Duration::from_secs(600),
        }
    }
}

/// What has happened to one request so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryState {
    attempts: u32,
    tier_waited: Duration,
}

impl RetryState {
    /// A request that has not been sent yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts a send. Call it before each attempt.
    pub fn begin_attempt(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
    }

    /// How many times the request has been sent, not counting an attempt
    /// that ended in a fallback to standard speed.
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// How long the request has waited in place for its speed tier.
    #[must_use]
    pub fn tier_waited(&self) -> Duration {
        self.tier_waited
    }
}

/// What to do about a failed attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Wait this long, then send the request again at the same speed.
    Wait(Duration),
    /// Send the request at standard speed now, and put the tier it was
    /// sent at into a cooldown.
    FallBack {
        /// Why the tier turned the request away.
        reason: SpeedFallbackReason,
        /// How long the cooldown lasts.
        cooldown: Duration,
    },
    /// Report the error. Nothing more is sent.
    GiveUp,
}

impl RetryPolicy {
    /// Decides what to do about `error`, the failure of the attempt just
    /// made at speed `sent`.
    ///
    /// `state` is the request's own: it is updated for what the decision
    /// implies (a wait in place is added to what the request has waited for
    /// its tier; a fallback gives the refused attempt back). `rng` supplies
    /// the jitter.
    pub fn decide(
        &self,
        state: &mut RetryState,
        error: &ProviderError,
        sent: &Speed,
        rng: &mut dyn Rng,
    ) -> Decision {
        if error.kind != ErrorKind::Transient {
            return Decision::GiveUp;
        }
        let attempts_left = state.attempts < self.max_attempts;

        let refused_by_tier = !sent.is_standard() && error.tier.as_ref() == Some(sent);
        if refused_by_tier && self.speed.fallback {
            if let Some(wait) = error.retry_after {
                let waited = state.tier_waited.saturating_add(wait);
                let allowed = !self.speed.short_wait.is_zero() && waited <= self.speed.short_wait;
                if attempts_left && allowed {
                    state.tier_waited = waited;
                    return Decision::Wait(wait);
                }
            }
            // The refusal says nothing against the request itself.
            state.attempts = state.attempts.saturating_sub(1);
            state.tier_waited = Duration::ZERO;
            let reason = match error.throttle {
                Some(Throttle::Overload) => SpeedFallbackReason::Overloaded,
                Some(Throttle::RateLimit) | None => SpeedFallbackReason::RateLimited,
            };
            return Decision::FallBack {
                reason,
                cooldown: self.cooldown(error.retry_after),
            };
        }

        if !attempts_left {
            return Decision::GiveUp;
        }
        match error.retry_after {
            Some(wait) if wait > self.max_wait => Decision::GiveUp,
            Some(wait) => Decision::Wait(wait),
            None => Decision::Wait(self.backoff(state.attempts, rng)),
        }
    }

    /// How long a speed tier's cooldown lasts: until the time the provider
    /// said the tier is available again, and for
    /// [`SpeedPolicy::cooldown`] when it said nothing.
    ///
    /// **This is the one place the length of a cooldown is decided.** A
    /// later change, such as a shortest cooldown so that a provider that
    /// keeps answering "in 25 seconds" cannot make a session switch speed
    /// every half minute, belongs here and nowhere else
    /// (`docs/decisions/0027-speed-tiers.md`, "Settled and open").
    #[must_use]
    pub fn cooldown(&self, provider_wait: Option<Duration>) -> Duration {
        provider_wait.unwrap_or(self.speed.cooldown)
    }

    /// The largest backoff wait after `failures` failed attempts:
    /// `base × factor^(failures − 1)`, at most `cap`.
    #[must_use]
    pub fn backoff_ceiling(&self, failures: u32) -> Duration {
        let exponent = failures.saturating_sub(1).min(64);
        let factor = if self.factor.is_finite() && self.factor >= 1.0 {
            self.factor
        } else {
            1.0
        };
        let ceiling = self.base.as_secs_f64() * factor.powi(exponent as i32);
        if ceiling.is_finite() && ceiling < self.cap.as_secs_f64() {
            Duration::try_from_secs_f64(ceiling).unwrap_or(self.cap)
        } else {
            self.cap
        }
    }

    /// A backoff wait with full jitter: anywhere from zero up to the
    /// ceiling, so that many clients that failed together do not come back
    /// together.
    fn backoff(&self, failures: u32, rng: &mut dyn Rng) -> Duration {
        self.backoff_ceiling(failures).mul_f64(rng.unit())
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::SeededRng;

    use super::*;
    use crate::error::Signal;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn transient() -> ProviderError {
        ProviderError::classified(Some(503), Signal::None, "unavailable", "Try later.")
    }

    fn rate_limited(wait: Option<u64>) -> ProviderError {
        ProviderError::classified(
            Some(429),
            Signal::RateLimit,
            "rate_limit_error",
            "Slow down.",
        )
        .with_retry_after(wait.map(secs))
    }

    fn tier_refusal(wait: Option<u64>) -> ProviderError {
        rate_limited(wait).for_tier(Speed::Fast)
    }

    /// A request that has been sent `attempts` times.
    fn sent(attempts: u32) -> RetryState {
        let mut state = RetryState::new();
        for _ in 0..attempts {
            state.begin_attempt();
        }
        state
    }

    fn decide(
        policy: &RetryPolicy,
        state: &mut RetryState,
        error: &ProviderError,
        speed: &Speed,
    ) -> Decision {
        policy.decide(state, error, speed, &mut SeededRng::new(7))
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.base, secs(1));
        assert!((policy.factor - 2.0).abs() < f64::EPSILON);
        assert_eq!(policy.cap, secs(30));
        assert_eq!(policy.max_wait, secs(120));
        assert_eq!(policy.idle_timeout, secs(90));
        assert_eq!(policy.first_event_timeout, secs(60));
        assert!(policy.speed.fallback);
        assert_eq!(policy.speed.short_wait, secs(20));
        assert_eq!(policy.speed.cooldown, secs(600));
    }

    #[test]
    fn only_transient_failures_are_retried() {
        let policy = RetryPolicy::default();
        for kind in ErrorKind::ALL {
            let error = ProviderError::new(kind, "code", "message");
            let decision = decide(&policy, &mut sent(1), &error, &Speed::Standard);
            if kind == ErrorKind::Transient {
                assert!(matches!(decision, Decision::Wait(_)), "{kind}");
            } else {
                assert_eq!(decision, Decision::GiveUp, "{kind}");
            }
        }
    }

    #[test]
    fn the_backoff_ceiling_doubles_and_stops_at_the_cap() {
        let policy = RetryPolicy::default();
        let ceilings: Vec<u64> = (1..=8)
            .map(|n| policy.backoff_ceiling(n).as_secs())
            .collect();
        assert_eq!(ceilings, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(policy.backoff_ceiling(0), secs(1), "before any failure");
        assert_eq!(policy.backoff_ceiling(u32::MAX), secs(30), "no overflow");

        let odd = RetryPolicy {
            base: Duration::from_millis(250),
            factor: 3.0,
            cap: secs(5),
            ..RetryPolicy::default()
        };
        let millis: Vec<u128> = (1..=5)
            .map(|n| odd.backoff_ceiling(n).as_millis())
            .collect();
        assert_eq!(millis, [250, 750, 2250, 5000, 5000]);

        // A factor that makes no sense does not shrink the wait or panic.
        for factor in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            let broken = RetryPolicy {
                factor,
                ..RetryPolicy::default()
            };
            assert_eq!(broken.backoff_ceiling(4), secs(1), "factor {factor}");
        }
    }

    #[test]
    fn a_backoff_wait_is_jittered_between_zero_and_the_ceiling() {
        let policy = RetryPolicy::default();
        let mut rng = SeededRng::new(11);
        for failures in 1..=4 {
            let ceiling = policy.backoff_ceiling(failures);
            let mut longest = Duration::ZERO;
            let mut shortest = Duration::MAX;
            for _ in 0..400 {
                let decision = policy.decide(
                    &mut sent(failures),
                    &transient(),
                    &Speed::Standard,
                    &mut rng,
                );
                let Decision::Wait(wait) = decision else {
                    panic!("{decision:?}");
                };
                assert!(wait <= ceiling, "{wait:?} over {ceiling:?}");
                longest = longest.max(wait);
                shortest = shortest.min(wait);
            }
            assert!(
                longest > ceiling.mul_f64(0.9),
                "never near the ceiling: {longest:?}"
            );
            assert!(
                shortest < ceiling.mul_f64(0.1),
                "never near zero: {shortest:?}"
            );
        }
        // The same generator gives the same waits.
        let first = decide(&policy, &mut sent(2), &transient(), &Speed::Standard);
        let second = decide(&policy, &mut sent(2), &transient(), &Speed::Standard);
        assert_eq!(first, second);
        let Decision::Wait(wait) = first else {
            panic!("{first:?}");
        };
        assert!(wait <= secs(2));
    }

    #[test]
    fn a_request_is_sent_at_most_max_attempts_times() {
        let policy = RetryPolicy::default();
        for attempts in 1..5 {
            assert!(
                matches!(
                    decide(&policy, &mut sent(attempts), &transient(), &Speed::Standard),
                    Decision::Wait(_)
                ),
                "after {attempts} attempts"
            );
        }
        assert_eq!(
            decide(&policy, &mut sent(5), &transient(), &Speed::Standard),
            Decision::GiveUp
        );
        let once = RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        };
        assert_eq!(
            decide(&once, &mut sent(1), &transient(), &Speed::Standard),
            Decision::GiveUp
        );
    }

    #[test]
    fn the_servers_wait_wins_over_backoff() {
        let policy = RetryPolicy::default();
        for wait in [0, 1, 45, 120] {
            assert_eq!(
                decide(
                    &policy,
                    &mut sent(1),
                    &rate_limited(Some(wait)),
                    &Speed::Standard
                ),
                Decision::Wait(secs(wait)),
                "{wait} s"
            );
        }
        // Also when it is longer than the backoff cap, and on the last try
        // but one.
        assert_eq!(
            decide(
                &policy,
                &mut sent(4),
                &rate_limited(Some(100)),
                &Speed::Standard
            ),
            Decision::Wait(secs(100))
        );
    }

    #[test]
    fn a_wait_over_the_maximum_is_not_slept() {
        let policy = RetryPolicy::default();
        assert_eq!(
            decide(
                &policy,
                &mut sent(1),
                &rate_limited(Some(121)),
                &Speed::Standard
            ),
            Decision::GiveUp
        );
        let patient = RetryPolicy {
            max_wait: secs(3600),
            ..RetryPolicy::default()
        };
        assert_eq!(
            decide(
                &patient,
                &mut sent(1),
                &rate_limited(Some(121)),
                &Speed::Standard
            ),
            Decision::Wait(secs(121))
        );
    }

    #[test]
    fn a_tiers_short_wait_is_waited_in_place() {
        let policy = RetryPolicy::default();
        let mut state = sent(1);
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(8)), &Speed::Fast),
            Decision::Wait(secs(8))
        );
        assert_eq!(state.tier_waited(), secs(8));
        assert_eq!(
            state.attempts(),
            1,
            "the wait in place is an ordinary retry"
        );
        // Exactly the budget is still within it.
        let mut state = sent(1);
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(20)), &Speed::Fast),
            Decision::Wait(secs(20))
        );
    }

    #[test]
    fn the_short_wait_is_a_budget_for_the_whole_request() {
        let policy = RetryPolicy::default();
        let mut state = sent(1);
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(8)), &Speed::Fast),
            Decision::Wait(secs(8))
        );
        state.begin_attempt();
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(8)), &Speed::Fast),
            Decision::Wait(secs(8))
        );
        state.begin_attempt();
        // 16 s waited; another 8 would make 24.
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(8)), &Speed::Fast),
            Decision::FallBack {
                reason: SpeedFallbackReason::RateLimited,
                cooldown: secs(8),
            }
        );
        assert_eq!(state.tier_waited(), Duration::ZERO);
        assert_eq!(state.attempts(), 2, "the refused attempt is given back");
    }

    #[test]
    fn a_long_or_unknown_wait_falls_back_at_once() {
        let policy = RetryPolicy::default();
        let mut state = sent(1);
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(21)), &Speed::Fast),
            Decision::FallBack {
                reason: SpeedFallbackReason::RateLimited,
                cooldown: secs(21),
            },
            "the cooldown ends when the provider said the tier is back"
        );
        assert_eq!(state.attempts(), 0);

        assert_eq!(
            decide(&policy, &mut sent(1), &tier_refusal(None), &Speed::Fast),
            Decision::FallBack {
                reason: SpeedFallbackReason::RateLimited,
                cooldown: secs(600),
            },
            "with no time from the provider, the configured cooldown"
        );

        // A wait far over the ordinary maximum is still a fallback, not an
        // error: standard speed is not limited by it.
        assert_eq!(
            decide(
                &policy,
                &mut sent(1),
                &tier_refusal(Some(7200)),
                &Speed::Fast
            ),
            Decision::FallBack {
                reason: SpeedFallbackReason::RateLimited,
                cooldown: secs(7200),
            }
        );
    }

    #[test]
    fn an_overloaded_tier_falls_back_and_says_so() {
        let policy = RetryPolicy::default();
        let overloaded =
            ProviderError::classified(Some(529), Signal::Overload, "overloaded_error", "Busy.")
                .for_tier(Speed::Fast);
        assert_eq!(
            decide(&policy, &mut sent(1), &overloaded, &Speed::Fast),
            Decision::FallBack {
                reason: SpeedFallbackReason::Overloaded,
                cooldown: secs(600),
            }
        );
    }

    #[test]
    fn a_tier_that_is_refused_on_the_last_attempt_falls_back_instead_of_failing() {
        let policy = RetryPolicy::default();
        let mut state = sent(5);
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(1)), &Speed::Fast),
            Decision::FallBack {
                reason: SpeedFallbackReason::RateLimited,
                cooldown: secs(1),
            }
        );
        assert_eq!(state.attempts(), 4, "standard speed gets a send of its own");
    }

    #[test]
    fn a_zero_short_wait_never_waits_in_place() {
        let policy = RetryPolicy {
            speed: SpeedPolicy {
                short_wait: Duration::ZERO,
                ..SpeedPolicy::default()
            },
            ..RetryPolicy::default()
        };
        for wait in [0, 1] {
            assert!(
                matches!(
                    decide(
                        &policy,
                        &mut sent(1),
                        &tier_refusal(Some(wait)),
                        &Speed::Fast
                    ),
                    Decision::FallBack { .. }
                ),
                "a wait of {wait} s"
            );
        }
    }

    #[test]
    fn with_fallback_off_a_tiers_refusal_is_an_ordinary_rate_limit() {
        let policy = RetryPolicy {
            speed: SpeedPolicy {
                fallback: false,
                ..SpeedPolicy::default()
            },
            ..RetryPolicy::default()
        };
        // The server's wait, short or long, as for any rate limit.
        let mut state = sent(1);
        assert_eq!(
            decide(&policy, &mut state, &tier_refusal(Some(45)), &Speed::Fast),
            Decision::Wait(secs(45))
        );
        assert_eq!(state.tier_waited(), Duration::ZERO);
        // Backoff when it names none.
        assert!(matches!(
            decide(&policy, &mut sent(1), &tier_refusal(None), &Speed::Fast),
            Decision::Wait(wait) if wait <= secs(1)
        ));
        // The maximum wait and the attempts hold.
        assert_eq!(
            decide(
                &policy,
                &mut sent(1),
                &tier_refusal(Some(121)),
                &Speed::Fast
            ),
            Decision::GiveUp
        );
        assert_eq!(
            decide(&policy, &mut sent(5), &tier_refusal(Some(1)), &Speed::Fast),
            Decision::GiveUp
        );
    }

    #[test]
    fn a_refusal_that_is_not_the_sent_tiers_is_the_models() {
        let policy = RetryPolicy::default();
        // The model's own limit, on a request sent at fast.
        assert_eq!(
            decide(&policy, &mut sent(1), &rate_limited(Some(45)), &Speed::Fast),
            Decision::Wait(secs(45))
        );
        // A tier's limit named on a request that was sent at standard, or at
        // another tier: nothing to fall back from.
        assert_eq!(
            decide(
                &policy,
                &mut sent(1),
                &tier_refusal(Some(45)),
                &Speed::Standard
            ),
            Decision::Wait(secs(45))
        );
        let flex = Speed::Other("flex".to_owned());
        assert_eq!(
            decide(&policy, &mut sent(1), &tier_refusal(Some(45)), &flex),
            Decision::Wait(secs(45))
        );
        // A provider's own tier name works like the well-known one.
        let flex_refusal = rate_limited(None).for_tier(flex.clone());
        assert!(matches!(
            decide(&policy, &mut sent(1), &flex_refusal, &flex),
            Decision::FallBack { .. }
        ));
    }

    #[test]
    fn a_tiers_failure_that_is_not_transient_is_reported() {
        let policy = RetryPolicy::default();
        let refused =
            ProviderError::classified(Some(400), Signal::None, "invalid_request_error", "No.")
                .for_tier(Speed::Fast);
        assert_eq!(
            decide(&policy, &mut sent(1), &refused, &Speed::Fast),
            Decision::GiveUp
        );
    }

    #[test]
    fn the_cooldown_is_the_providers_time_or_the_configured_one() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.cooldown(None), secs(600));
        assert_eq!(policy.cooldown(Some(secs(25))), secs(25));
        assert_eq!(policy.cooldown(Some(secs(7200))), secs(7200));
        let own = RetryPolicy {
            speed: SpeedPolicy {
                cooldown: secs(90),
                ..SpeedPolicy::default()
            },
            ..RetryPolicy::default()
        };
        assert_eq!(own.cooldown(None), secs(90));
        assert_eq!(own.cooldown(Some(secs(25))), secs(25));
    }
}
