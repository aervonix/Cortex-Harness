//! Time behind a trait (`docs/testing-and-ci.md`, "No real waiting in
//! tests").
//!
//! Production code never reads the system clock and never sleeps directly.
//! Async code waits on the runtime's timers, which tests pause. Everything
//! else takes a [`Clock`]: [`SystemClock`] when the harness runs, and
//! [`FakeClock`] in tests, where a 30-second wait takes no time at all.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::sync::lock;
use crate::time::UtcTime;

/// A source of time.
///
/// It gives two different things, and code must pick the right one:
///
/// - [`now`](Clock::now) is the date and time. It can jump in either
///   direction when the machine's clock is corrected. Use it for timestamps
///   and for comparing with a time someone else wrote down, such as an
///   expiry.
/// - [`monotonic`](Clock::monotonic) only ever moves forward. Use it to
///   measure how long something took and to decide when a timeout is over.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The date and time.
    fn now(&self) -> UtcTime;

    /// The time since this clock's own starting point, which is fixed and
    /// otherwise arbitrary. Subtract two readings to measure a span.
    fn monotonic(&self) -> Duration;

    /// Blocks the calling thread for `duration`.
    ///
    /// Only for code that runs on the blocking pool, such as the retry of a
    /// file replace. Async code waits on the runtime's timers instead: a
    /// sleeping core thread stalls every session.
    fn sleep(&self, duration: Duration);
}

/// A clock shared between owners.
pub type SharedClock = Arc<dyn Clock>;

/// The operating system's clock.
#[derive(Debug, Clone)]
pub struct SystemClock {
    started: Instant,
}

impl SystemClock {
    /// The system clock. Its monotonic time starts at zero now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> UtcTime {
        // A system clock outside the years 0000 to 9999 is a broken clock.
        // The epoch is as good an answer as any, and it is not a panic.
        UtcTime::from_system(SystemTime::now()).unwrap_or(UtcTime::UNIX_EPOCH)
    }

    fn monotonic(&self) -> Duration {
        self.started.elapsed()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "this is the one place that really sleeps; callers are on the blocking pool"
    )]
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A clock that moves only when a test moves it: the fake.
///
/// Clones share one time, so a test keeps a handle and gives another to the
/// code under test. [`sleep`](Clock::sleep) returns at once and moves the
/// clock forward by the time asked for, so code that waits and retries runs
/// instantly and the test can read back how long it "waited".
#[derive(Debug, Clone)]
pub struct FakeClock {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Debug)]
struct FakeState {
    now: UtcTime,
    monotonic: Duration,
    sleeps: Vec<Duration>,
}

impl FakeClock {
    /// A clock that reads `start` until it is moved.
    #[must_use]
    pub fn new(start: UtcTime) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                now: start,
                monotonic: Duration::ZERO,
                sleeps: Vec::new(),
            })),
        }
    }

    /// A clock that starts at the given RFC 3339 time.
    pub fn starting_at(start: &str) -> Result<Self, crate::time::TimeError> {
        UtcTime::parse(start).map(Self::new)
    }

    /// Moves the clock forward: the date and the monotonic time together, as
    /// when time passes. The date stops at the end of the year 9999.
    pub fn advance(&self, duration: Duration) {
        lock(&self.state).advance(duration);
    }

    /// Sets the date and time without touching the monotonic time, as when
    /// the machine's clock is corrected. It may go backwards.
    pub fn set_now(&self, now: UtcTime) {
        lock(&self.state).now = now;
    }

    /// Every sleep asked of this clock so far, in order.
    #[must_use]
    pub fn sleeps(&self) -> Vec<Duration> {
        lock(&self.state).sleeps.clone()
    }

    /// The total of every sleep asked of this clock so far.
    #[must_use]
    pub fn slept(&self) -> Duration {
        lock(&self.state).sleeps.iter().sum()
    }
}

impl FakeState {
    fn advance(&mut self, duration: Duration) {
        self.monotonic = self.monotonic.saturating_add(duration);
        if let Some(later) = self.now.checked_add(duration) {
            self.now = later;
        }
    }
}

impl Default for FakeClock {
    /// A clock that starts at 2026-01-01T00:00:00Z.
    fn default() -> Self {
        Self::new(UtcTime::from_unix(1_767_225_600, 0).unwrap_or(UtcTime::UNIX_EPOCH))
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UtcTime {
        lock(&self.state).now
    }

    fn monotonic(&self) -> Duration {
        lock(&self.state).monotonic
    }

    fn sleep(&self, duration: Duration) {
        let mut state = lock(&self.state);
        state.sleeps.push(duration);
        state.advance(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_clock_moves_only_when_told() {
        let clock = FakeClock::starting_at("2026-11-12T10:15:02Z").unwrap();
        assert_eq!(clock.now().to_string(), "2026-11-12T10:15:02Z");
        assert_eq!(clock.monotonic(), Duration::ZERO);
        assert_eq!(clock.now(), clock.now());

        clock.advance(Duration::from_secs(90));
        assert_eq!(clock.now().to_string(), "2026-11-12T10:16:32Z");
        assert_eq!(clock.monotonic(), Duration::from_secs(90));
        assert!(clock.sleeps().is_empty(), "advancing is not sleeping");
    }

    #[test]
    fn a_sleep_takes_no_real_time_and_is_recorded() {
        let clock = FakeClock::default();
        assert_eq!(clock.now().to_string(), "2026-01-01T00:00:00Z");
        let handle: SharedClock = Arc::new(clock.clone());

        handle.sleep(Duration::from_secs(30));
        handle.sleep(Duration::from_millis(250));

        assert_eq!(
            clock.sleeps(),
            [Duration::from_secs(30), Duration::from_millis(250)]
        );
        assert_eq!(clock.slept(), Duration::from_millis(30_250));
        assert_eq!(clock.monotonic(), Duration::from_millis(30_250));
        assert_eq!(clock.now().to_string(), "2026-01-01T00:00:30.250Z");
    }

    #[test]
    fn the_date_can_jump_back_while_monotonic_time_cannot() {
        let clock = FakeClock::default();
        clock.advance(Duration::from_secs(10));
        let before = clock.now();
        clock.set_now(UtcTime::UNIX_EPOCH);
        assert!(clock.now() < before);
        assert_eq!(clock.now().duration_since(before), None);
        assert_eq!(clock.monotonic(), Duration::from_secs(10));
        clock.advance(Duration::from_secs(1));
        assert_eq!(clock.monotonic(), Duration::from_secs(11));
        assert_eq!(clock.now().unix_secs(), 1);
    }

    #[test]
    fn the_fake_clock_does_not_run_off_the_end_of_time() {
        let clock = FakeClock::starting_at("9999-12-31T23:59:59Z").unwrap();
        clock.advance(Duration::from_secs(5));
        assert_eq!(clock.now().to_string(), "9999-12-31T23:59:59Z");
        assert_eq!(clock.monotonic(), Duration::from_secs(5));
        clock.advance(Duration::MAX);
        assert_eq!(clock.monotonic(), Duration::MAX);
    }

    #[test]
    fn the_system_clock_reads_a_plausible_time_and_does_not_go_back() {
        let clock = SystemClock::new();
        let first = clock.monotonic();
        let second = clock.monotonic();
        assert!(second >= first);
        // After this code was written, and before the year 2200.
        let now = clock.now();
        assert!(
            now > UtcTime::parse("2026-01-01T00:00:00Z").unwrap(),
            "{now}"
        );
        assert!(
            now < UtcTime::parse("2200-01-01T00:00:00Z").unwrap(),
            "{now}"
        );
        // A sleep of nothing returns.
        SystemClock::default().sleep(Duration::ZERO);
    }
}
