//! Making identifiers.
//!
//! The protocol's identifiers are opaque strings to a client
//! ([`cortex_protocol::ids`]). The harness makes two sorts:
//!
//! - **Unique ids** ([`IdGenerator`]), for things that outlive a process and
//!   must never collide: sessions, leases, artifacts, approvals. They are
//!   time-ordered, so sorting sessions by id sorts them by creation time
//!   (`docs/architecture/07-sessions-and-context.md`).
//! - **Counted ids** ([`IdCounter`]), for things numbered inside one session:
//!   turns, items, log entries.
//!
//! Both are lowercase ASCII letters, digits and `_`, so that an id can be a
//! file or directory name on every OS, including one that ignores case.

use std::sync::Mutex;

use cortex_protocol::{ApprovalId, ArtifactId, LeaseId, SessionId};

use crate::clock::SharedClock;
use crate::rng::Rng;
use crate::sync::lock;
use crate::time::UtcTime;

/// The 32 digits of a unique id, in ASCII order, so that ids sort as the
/// numbers they encode. The letters `i`, `l`, `o` and `u` are left out: the
/// first three are easily misread as digits when someone types an id.
const DIGITS: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// Digits of the time part: 50 bits of milliseconds, enough until the year
/// 37648.
const TIME_DIGITS: usize = 10;
/// Digits of the random part: 60 bits.
const RANDOM_DIGITS: usize = 12;
const RANDOM_LIMIT: u64 = 1 << (5 * RANDOM_DIGITS);

/// Makes unique, time-ordered ids.
///
/// An id is a prefix that says what it names (`s_` for a session), then ten
/// digits of the creation time in milliseconds, then twelve random digits:
/// `s_01kcxm2f9w3h8t0z5n6qb7dre4`.
///
/// **Ordered.** Ids from one generator sort in the order they were made,
/// even several in one millisecond and even if the clock is set back: a new
/// id is never smaller than the one before. Ids from different processes
/// sort by creation time to the millisecond.
///
/// **Unique.** Two processes collide only if they make an id in the same
/// millisecond and draw the same 60 random bits.
#[derive(Debug)]
pub struct IdGenerator {
    clock: SharedClock,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    rng: Box<dyn Rng>,
    /// The time and random parts of the last id made.
    last: Option<(u64, u64)>,
}

impl IdGenerator {
    /// A generator that reads the time from `clock` and randomness from `rng`.
    #[must_use]
    pub fn new(clock: SharedClock, rng: Box<dyn Rng>) -> Self {
        Self {
            clock,
            state: Mutex::new(State { rng, last: None }),
        }
    }

    /// A new id that starts with `prefix`.
    #[must_use]
    pub fn unique(&self, prefix: &str) -> String {
        let now = u64::try_from(self.clock.now().unix_millis()).unwrap_or(0);
        let mut state = lock(&self.state);
        let (time, random) = match state.last {
            // Same millisecond, or the clock went back: stay on the last
            // id's time and count the random part up, so order is kept.
            Some((last_time, last_random)) if now <= last_time => {
                if last_random + 1 < RANDOM_LIMIT {
                    (last_time, last_random + 1)
                } else {
                    (last_time + 1, state.rng.below(RANDOM_LIMIT))
                }
            }
            _ => (now, state.rng.below(RANDOM_LIMIT)),
        };
        state.last = Some((time, random));
        drop(state);

        let mut id = String::with_capacity(prefix.len() + TIME_DIGITS + RANDOM_DIGITS);
        id.push_str(prefix);
        push_digits(&mut id, time, TIME_DIGITS);
        push_digits(&mut id, random, RANDOM_DIGITS);
        id
    }

    /// A new session id: `s_…`.
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        SessionId::new(self.unique("s_"))
    }

    /// A new lease id: `l_…`.
    #[must_use]
    pub fn lease_id(&self) -> LeaseId {
        LeaseId::new(self.unique("l_"))
    }

    /// A new artifact id: `a_…`.
    #[must_use]
    pub fn artifact_id(&self) -> ArtifactId {
        ArtifactId::new(self.unique("a_"))
    }

    /// A new approval id: `ap_…`.
    #[must_use]
    pub fn approval_id(&self) -> ApprovalId {
        ApprovalId::new(self.unique("ap_"))
    }
}

/// When a unique id was made, to the millisecond. `None` for a string that
/// is not `prefix` followed by a unique id's 22 digits.
///
/// For the harness's own use, such as listing sessions before their logs are
/// read. Clients read nothing into an id.
#[must_use]
pub fn created_at(id: &str, prefix: &str) -> Option<UtcTime> {
    let body = id.strip_prefix(prefix)?.as_bytes();
    if body.len() != TIME_DIGITS + RANDOM_DIGITS {
        return None;
    }
    let mut parts = [0u64; 2];
    for (part, digits) in parts
        .iter_mut()
        .zip([&body[..TIME_DIGITS], &body[TIME_DIGITS..]])
    {
        for digit in digits {
            let value = DIGITS.iter().position(|candidate| candidate == digit)?;
            *part = (*part << 5) | value as u64;
        }
    }
    UtcTime::from_unix_millis(i64::try_from(parts[0]).ok()?)
}

/// Appends `value` as exactly `count` digits, most significant first.
fn push_digits(out: &mut String, value: u64, count: usize) {
    for position in (0..count).rev() {
        let digit = (value >> (5 * position)) & 31;
        out.push(char::from(DIGITS[digit as usize]));
    }
}

/// Makes ids that count up: `t_1`, `t_2`, `t_3`.
///
/// For things numbered inside one session. The ids are unique only within
/// whatever owns the counter.
#[derive(Debug, Clone)]
pub struct IdCounter {
    prefix: String,
    width: usize,
    next: u64,
}

impl IdCounter {
    /// A counter whose first id is `<prefix>1`.
    #[must_use]
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            width: 0,
            next: 1,
        }
    }

    /// Pads the number with zeros to at least `width` digits: `e_0007`. Ids
    /// then sort as text until the number outgrows the width.
    #[must_use]
    pub fn padded(mut self, width: usize) -> Self {
        self.width = width;
        self
    }

    /// Continues after `last`, the highest number already used, as when a
    /// session is resumed.
    #[must_use]
    pub fn after(mut self, last: u64) -> Self {
        self.next = last.saturating_add(1);
        self
    }

    /// The next id, as any of the protocol's id types or as a `String`.
    pub fn next_id<T: From<String>>(&mut self) -> T {
        let number = self.next;
        self.next = self.next.saturating_add(1);
        T::from(format!(
            "{}{number:0width$}",
            self.prefix,
            width = self.width
        ))
    }

    /// The number the next id will carry.
    #[must_use]
    pub fn peek(&self) -> u64 {
        self.next
    }

    /// The number in an id this counter made, or one like it. `None` for
    /// anything else.
    #[must_use]
    pub fn number_of(&self, id: &str) -> Option<u64> {
        let digits = id.strip_prefix(self.prefix.as_str())?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use cortex_protocol::{ItemId, TurnId};

    use super::*;
    use crate::clock::{Clock, FakeClock};
    use crate::rng::SeededRng;

    fn generator(clock: &FakeClock, seed: u64) -> IdGenerator {
        IdGenerator::new(Arc::new(clock.clone()), Box::new(SeededRng::new(seed)))
    }

    fn is_file_name_safe(id: &str) -> bool {
        id.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    }

    #[test]
    fn an_id_has_its_prefix_its_time_and_a_fixed_length() {
        let clock = FakeClock::starting_at("2026-11-12T10:15:02.250Z").unwrap();
        let ids = generator(&clock, 1);
        let session = ids.session_id();
        assert!(session.as_str().starts_with("s_"), "{session}");
        assert_eq!(session.as_str().len(), 2 + 22);
        assert!(is_file_name_safe(session.as_str()), "{session}");
        assert_eq!(created_at(session.as_str(), "s_"), Some(clock.now()));

        assert!(ids.lease_id().as_str().starts_with("l_"));
        assert!(ids.artifact_id().as_str().starts_with("a_"));
        let approval = ids.approval_id();
        assert!(approval.as_str().starts_with("ap_"));
        assert_eq!(created_at(approval.as_str(), "ap_"), Some(clock.now()));
    }

    #[test]
    fn the_same_clock_and_seed_give_the_same_ids() {
        let clock = FakeClock::default();
        let first = generator(&clock, 7);
        let second = generator(&clock, 7);
        let other = generator(&clock, 8);
        assert_eq!(first.unique("s_"), second.unique("s_"));
        assert_ne!(first.unique("s_"), other.unique("s_"));
    }

    #[test]
    fn ids_sort_in_the_order_they_were_made() {
        let clock = FakeClock::default();
        let ids = generator(&clock, 3);
        let mut made = Vec::new();
        for round in 0..200u64 {
            // Several in one millisecond, then time passes, and now and
            // then the clock is set back.
            made.push(ids.unique("s_"));
            made.push(ids.unique("s_"));
            match round % 7 {
                0 => clock.set_now(clock.now().checked_sub(Duration::from_secs(5)).unwrap()),
                1 => {}
                _ => clock.advance(Duration::from_millis(round % 3)),
            }
        }
        for pair in made.windows(2) {
            assert!(pair[0] < pair[1], "{} then {}", pair[0], pair[1]);
        }
        let distinct: std::collections::BTreeSet<_> = made.iter().collect();
        assert_eq!(distinct.len(), made.len());
    }

    #[test]
    fn later_ids_sort_after_earlier_ones_across_generators() {
        let clock = FakeClock::default();
        let early = generator(&clock, 1).unique("s_");
        clock.advance(Duration::from_millis(1));
        let late = generator(&clock, 2).unique("s_");
        assert!(early < late);
        // The time part carries into every digit.
        clock.advance(Duration::from_secs(400 * 365 * 86_400));
        let much_later = generator(&clock, 3).unique("s_");
        assert!(late < much_later);
        assert_eq!(created_at(&much_later, "s_"), Some(clock.now()));
    }

    #[test]
    fn the_random_part_rolling_over_moves_to_the_next_millisecond() {
        let clock = FakeClock::default();
        let ids = generator(&clock, 5);
        let first = ids.unique("s_");
        let millis = u64::try_from(clock.now().unix_millis()).unwrap();
        lock(&ids.state).last = Some((millis, RANDOM_LIMIT - 1));
        let next = ids.unique("s_");
        assert!(next > first);
        assert_eq!(
            created_at(&next, "s_"),
            clock.now().checked_add(Duration::from_millis(1))
        );
    }

    #[test]
    fn a_clock_before_1970_still_gives_ids() {
        let clock = FakeClock::starting_at("1960-01-01T00:00:00Z").unwrap();
        let ids = generator(&clock, 5);
        let first = ids.unique("s_");
        let second = ids.unique("s_");
        assert!(first < second);
        assert_eq!(created_at(&first, "s_"), Some(UtcTime::UNIX_EPOCH));
    }

    #[test]
    fn only_a_unique_id_has_a_creation_time() {
        assert_eq!(created_at("s_01JCX4T9Q2", "s_"), None, "too short");
        assert_eq!(created_at("t_1", "t_"), None);
        assert_eq!(created_at("s_0000000000000000000000", "x_"), None);
        assert_eq!(
            created_at("s_0000000000000000000000", "s_"),
            Some(UtcTime::UNIX_EPOCH)
        );
        assert_eq!(
            created_at("s_000000000000000000000i", "s_"),
            None,
            "not a digit"
        );
        assert_eq!(
            created_at("s_000000000000000000000A", "s_"),
            None,
            "uppercase"
        );
        assert_eq!(
            created_at("s_zzzzzzzzzz000000000000", "s_"),
            None,
            "past 9999"
        );
    }

    #[test]
    fn digits_are_in_ascii_order() {
        assert!(DIGITS.windows(2).all(|pair| pair[0] < pair[1]));
        let mut text = String::new();
        push_digits(&mut text, 0, 3);
        push_digits(&mut text, 31, 1);
        push_digits(&mut text, 32, 2);
        assert_eq!(text, "000z10");
    }

    #[test]
    fn a_counter_counts_from_one() {
        let mut turns = IdCounter::new("t_");
        let first: TurnId = turns.next_id();
        let second: TurnId = turns.next_id();
        assert_eq!(first.as_str(), "t_1");
        assert_eq!(second.as_str(), "t_2");
        assert_eq!(turns.peek(), 3);
        assert_eq!(turns.number_of("t_2"), Some(2));
        assert_eq!(turns.number_of("i_2"), None);
        assert_eq!(turns.number_of("t_"), None);
        assert_eq!(turns.number_of("t_+2"), None);
        assert_eq!(turns.number_of("t_2x"), None);
    }

    #[test]
    fn a_counter_can_pad_and_resume() {
        let mut entries = IdCounter::new("e_").padded(4).after(6);
        let entry: String = entries.next_id();
        assert_eq!(entry, "e_0007");
        assert_eq!(entries.number_of(&entry), Some(7));
        let mut entries = entries.after(9_999);
        assert_eq!(entries.next_id::<String>(), "e_10000");

        let mut items = IdCounter::new("i_").after(u64::MAX);
        let last: ItemId = items.next_id();
        assert_eq!(last.as_str(), format!("i_{}", u64::MAX));
        assert_eq!(items.peek(), u64::MAX, "it stops, and does not wrap to 0");
    }
}
