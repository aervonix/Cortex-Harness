//! The frame scheduler: when to draw (`docs/architecture/09-tui.md`, "Frame
//! scheduling").
//!
//! - A key is drawn at once. It never waits for the frame timer.
//! - What streams in is drawn at most 60 times a second; whatever arrives
//!   between two frames goes into the next one.
//! - While something runs, an animation tick draws at the rate it asks for.
//! - When nothing has changed, nothing is scheduled, and the loop sleeps
//!   until something happens.
//!
//! The scheduler holds no clock and starts no timer. The loop tells it the
//! time with each call and asks it when to wake, so it is tested with times
//! made up by the test.

use std::time::{Duration, Instant};

/// The shortest time between two frames drawn for streamed content: 60 a
/// second.
pub const STREAM_INTERVAL: Duration = Duration::from_micros(16_667);

/// When the loop should draw next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// Draw now.
    Now,
    /// Draw at this time, unless something comes first.
    At(Instant),
    /// Nothing to draw: sleep until something happens.
    Idle,
}

/// Decides when frames are drawn. See the [module documentation](self).
#[derive(Debug, Clone)]
pub struct Scheduler {
    interval: Duration,
    last: Option<Instant>,
    input: bool,
    stream: bool,
    tick: Option<(Duration, Instant)>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// A scheduler that draws streamed content at most 60 times a second.
    #[must_use]
    pub fn new() -> Self {
        Self::with_interval(STREAM_INTERVAL)
    }

    /// A scheduler that draws streamed content at most once in each
    /// `interval`.
    #[must_use]
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
            input: false,
            stream: false,
            tick: None,
        }
    }

    /// Something the user did changed the screen: the next frame is drawn
    /// at once.
    pub fn input(&mut self) {
        self.input = true;
    }

    /// Streamed content changed the screen: the next frame is drawn when
    /// the frame interval allows.
    pub fn stream(&mut self) {
        self.stream = true;
    }

    /// Something is running that animates: draw every `every` from `now` on.
    /// `None` stops the ticks.
    pub fn animate(&mut self, every: Option<Duration>, now: Instant) {
        self.tick = match (every, self.tick) {
            (None, _) => None,
            (Some(every), Some((current, next))) if current == every => Some((every, next)),
            (Some(every), _) => Some((every, now + every)),
        };
    }

    /// When the loop should draw next, at `now`.
    #[must_use]
    pub fn wake(&self, now: Instant) -> Wake {
        if self.input {
            return Wake::Now;
        }
        let stream = self.stream.then(|| match self.last {
            Some(last) => last + self.interval,
            None => now,
        });
        let tick = self.tick.map(|(_, next)| next);
        match (stream, tick) {
            (None, None) => Wake::Idle,
            (Some(at), None) | (None, Some(at)) => at_or_now(at, now),
            (Some(a), Some(b)) => at_or_now(a.min(b), now),
        }
    }

    /// A frame was drawn at `now`: everything waiting went into it.
    ///
    /// A frame drawn for streamed content a little after it was due keeps
    /// the cadence: the next is due one interval after this one was, not
    /// after it was drawn, so that the loop's own wake-up delay does not
    /// stretch every interval.
    pub fn drawn(&mut self, now: Instant) {
        let due = self.last.map(|last| last + self.interval);
        self.last = Some(match due {
            Some(due) if self.stream && !self.input && now >= due && now < due + self.interval => {
                due
            }
            _ => now,
        });
        self.input = false;
        self.stream = false;
        if let Some((every, next)) = &mut self.tick {
            while *next <= now {
                *next += *every;
            }
        }
    }
}

fn at_or_now(at: Instant, now: Instant) -> Wake {
    if at <= now { Wake::Now } else { Wake::At(at) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn idle_schedules_nothing() {
        let start = Instant::now();
        let mut scheduler = Scheduler::new();
        assert_eq!(scheduler.wake(start), Wake::Idle);
        scheduler.drawn(start);
        assert_eq!(scheduler.wake(start + ms(1000)), Wake::Idle);
    }

    #[test]
    fn a_key_is_drawn_at_once_even_right_after_a_frame() {
        let start = Instant::now();
        let mut scheduler = Scheduler::new();
        scheduler.stream();
        scheduler.drawn(start);
        scheduler.stream();
        scheduler.input();
        assert_eq!(scheduler.wake(start + ms(1)), Wake::Now);
        scheduler.drawn(start + ms(1));
        assert_eq!(scheduler.wake(start + ms(1)), Wake::Idle);
    }

    #[test]
    fn streamed_content_is_drawn_at_most_sixty_times_a_second() {
        let start = Instant::now();
        let mut scheduler = Scheduler::new();
        scheduler.stream();
        assert_eq!(scheduler.wake(start), Wake::Now);
        scheduler.drawn(start);
        for n in 1..10 {
            scheduler.stream();
            assert_eq!(
                scheduler.wake(start + ms(n)),
                Wake::At(start + STREAM_INTERVAL)
            );
        }
        assert_eq!(scheduler.wake(start + STREAM_INTERVAL), Wake::Now);
        scheduler.drawn(start + ms(20));
        assert_eq!(scheduler.wake(start + ms(21)), Wake::Idle);
    }

    #[test]
    fn a_late_streamed_frame_keeps_the_cadence() {
        let start = Instant::now();
        let mut scheduler = Scheduler::new();
        scheduler.stream();
        scheduler.drawn(start);
        // Drawn a millisecond late: the next is still due one interval
        // after this one was.
        scheduler.stream();
        scheduler.drawn(start + STREAM_INTERVAL + ms(1));
        scheduler.stream();
        assert_eq!(
            scheduler.wake(start + STREAM_INTERVAL + ms(2)),
            Wake::At(start + STREAM_INTERVAL * 2)
        );
        // A key resets it: the next streamed frame is an interval after the
        // key's.
        scheduler.input();
        scheduler.drawn(start + STREAM_INTERVAL * 2 + ms(5));
        scheduler.stream();
        assert_eq!(
            scheduler.wake(start + STREAM_INTERVAL * 2 + ms(6)),
            Wake::At(start + STREAM_INTERVAL * 3 + ms(5))
        );
        // Very late: the cadence starts again from now.
        scheduler.drawn(start + ms(500));
        scheduler.stream();
        assert_eq!(
            scheduler.wake(start + ms(501)),
            Wake::At(start + ms(500) + STREAM_INTERVAL)
        );
    }

    #[test]
    fn an_animation_ticks_until_it_is_stopped() {
        let start = Instant::now();
        let mut scheduler = Scheduler::new();
        scheduler.animate(Some(ms(100)), start);
        assert_eq!(scheduler.wake(start), Wake::At(start + ms(100)));
        // Asking again for the same rate does not push the tick back.
        scheduler.animate(Some(ms(100)), start + ms(50));
        assert_eq!(scheduler.wake(start + ms(50)), Wake::At(start + ms(100)));
        assert_eq!(scheduler.wake(start + ms(100)), Wake::Now);
        scheduler.drawn(start + ms(250));
        assert_eq!(scheduler.wake(start + ms(250)), Wake::At(start + ms(300)));
        scheduler.stream();
        assert_eq!(
            scheduler.wake(start + ms(251)),
            Wake::At(start + ms(250) + STREAM_INTERVAL)
        );
        scheduler.animate(None, start + ms(260));
        scheduler.drawn(start + ms(270));
        assert_eq!(scheduler.wake(start + ms(270)), Wake::Idle);
    }
}
