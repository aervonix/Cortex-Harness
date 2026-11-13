//! [`UtcTime`]: a moment in time, and its RFC 3339 form.
//!
//! The protocol writes every time as an RFC 3339 string in UTC
//! ([`cortex_protocol::Timestamp`]). This is the value behind that string:
//! what the [`Clock`](crate::Clock) returns, what a lease's expiry is
//! compared with, and what a session's first id is made from.
//!
//! The parser is strict, because a time can be part of an enforced value
//! (an expiry). It reads what RFC 3339 requires and nothing it merely
//! allows: see [`UtcTime::parse`].

use std::time::{Duration, SystemTime};

use cortex_protocol::Timestamp;

const NANOS_PER_SEC: u32 = 1_000_000_000;
const SECS_PER_DAY: i64 = 86_400;
/// 0000-01-01T00:00:00Z, the first moment RFC 3339 can write.
const MIN_SECS: i64 = -62_167_219_200;
/// 9999-12-31T23:59:59Z, the last second RFC 3339 can write.
const MAX_SECS: i64 = 253_402_300_799;

/// A moment in time, to the nanosecond, between the years 0000 and 9999.
///
/// Ordered by time. Leap seconds are not represented, as in Unix time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTime {
    /// Whole seconds since 1970-01-01T00:00:00Z.
    secs: i64,
    /// The rest, below one second.
    nanos: u32,
}

/// Why a string is not a time this crate reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    /// The text does not have the form `YYYY-MM-DDTHH:MM:SS[.fraction]Z`, or
    /// the same with a numeric offset.
    #[error(
        "`{0}` is not an RFC 3339 time: expected the form 2026-11-12T10:15:02Z, with `T` and \
         `Z` in capitals, or a numeric offset such as +01:00"
    )]
    Malformed(String),
    /// The form is right and a field is out of range: month 13, hour 24,
    /// 30 February, a leap second.
    #[error("`{0}` is not a real time: {1} is out of range")]
    OutOfRange(String, &'static str),
}

impl UtcTime {
    /// 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: Self = Self { secs: 0, nanos: 0 };

    /// A time from seconds and nanoseconds since the Unix epoch. `None` when
    /// the time is outside the years 0000 to 9999 or `nanos` is a second or
    /// more.
    #[must_use]
    pub const fn from_unix(secs: i64, nanos: u32) -> Option<Self> {
        if secs < MIN_SECS || secs > MAX_SECS || nanos >= NANOS_PER_SEC {
            None
        } else {
            Some(Self { secs, nanos })
        }
    }

    /// A time from milliseconds since the Unix epoch.
    #[must_use]
    pub const fn from_unix_millis(millis: i64) -> Option<Self> {
        let secs = millis.div_euclid(1000);
        let nanos = millis.rem_euclid(1000) as u32 * 1_000_000;
        Self::from_unix(secs, nanos)
    }

    /// The operating system's time as a `UtcTime`. `None` when it is outside
    /// the years 0000 to 9999.
    #[must_use]
    pub fn from_system(time: SystemTime) -> Option<Self> {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(after) => {
                Self::from_unix(i64::try_from(after.as_secs()).ok()?, after.subsec_nanos())
            }
            Err(before) => {
                let before = before.duration();
                let secs = i64::try_from(before.as_secs()).ok()?;
                if before.subsec_nanos() == 0 {
                    Self::from_unix(secs.checked_neg()?, 0)
                } else {
                    Self::from_unix(
                        secs.checked_neg()?.checked_sub(1)?,
                        NANOS_PER_SEC - before.subsec_nanos(),
                    )
                }
            }
        }
    }

    /// Whole seconds since the Unix epoch. Negative before 1970.
    #[must_use]
    pub const fn unix_secs(self) -> i64 {
        self.secs
    }

    /// The part of the time below one second, in nanoseconds.
    #[must_use]
    pub const fn subsec_nanos(self) -> u32 {
        self.nanos
    }

    /// Whole milliseconds since the Unix epoch, rounded towards the past.
    #[must_use]
    pub const fn unix_millis(self) -> i64 {
        self.secs * 1000 + (self.nanos / 1_000_000) as i64
    }

    /// This time moved into the future. `None` past the year 9999.
    #[must_use]
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        let mut secs = self
            .secs
            .checked_add(i64::try_from(duration.as_secs()).ok()?)?;
        let mut nanos = self.nanos + duration.subsec_nanos();
        if nanos >= NANOS_PER_SEC {
            nanos -= NANOS_PER_SEC;
            secs = secs.checked_add(1)?;
        }
        Self::from_unix(secs, nanos)
    }

    /// This time moved into the past. `None` before the year 0000.
    #[must_use]
    pub fn checked_sub(self, duration: Duration) -> Option<Self> {
        let mut secs = self
            .secs
            .checked_sub(i64::try_from(duration.as_secs()).ok()?)?;
        let nanos = if self.nanos >= duration.subsec_nanos() {
            self.nanos - duration.subsec_nanos()
        } else {
            secs = secs.checked_sub(1)?;
            self.nanos + NANOS_PER_SEC - duration.subsec_nanos()
        };
        Self::from_unix(secs, nanos)
    }

    /// How long after `earlier` this time is. `None` when `earlier` is in
    /// fact later: a wall clock can be set back, so a caller has to decide
    /// what that means for it.
    #[must_use]
    pub fn duration_since(self, earlier: Self) -> Option<Duration> {
        if self < earlier {
            return None;
        }
        let mut secs = self.secs - earlier.secs;
        let nanos = if self.nanos >= earlier.nanos {
            self.nanos - earlier.nanos
        } else {
            secs -= 1;
            self.nanos + NANOS_PER_SEC - earlier.nanos
        };
        Some(Duration::new(u64::try_from(secs).ok()?, nanos))
    }

    /// The time as the protocol writes it: `2026-11-12T10:15:02Z`.
    ///
    /// A time with no part below one second has no fraction. Otherwise the
    /// fraction has 3, 6 or 9 digits, the fewest that lose nothing.
    #[must_use]
    pub fn to_timestamp(self) -> Timestamp {
        Timestamp::new(self.to_string())
    }

    /// Reads an RFC 3339 time.
    ///
    /// Accepted: `YYYY-MM-DDTHH:MM:SS`, an optional fraction of one to nine
    /// digits, then `Z` or a numeric offset (`+01:00`, `-08:00`), which is
    /// converted to UTC.
    ///
    /// Refused, although RFC 3339 allows a reader to accept them: a lowercase
    /// `t` or `z`, a space in place of the `T`, a leap second (`:60`), and a
    /// fraction finer than a nanosecond. A time that cannot be read is an
    /// error, never a guess, because an expiry is something the harness
    /// enforces.
    pub fn parse(text: &str) -> Result<Self, TimeError> {
        let malformed = || TimeError::Malformed(text.to_owned());
        let range = |field| TimeError::OutOfRange(text.to_owned(), field);
        let bytes = text.as_bytes();
        // The shortest form is 20 bytes: 2026-11-12T10:15:02Z.
        if bytes.len() < 20 {
            return Err(malformed());
        }
        let digits = |from: usize, count: usize| -> Result<i64, TimeError> {
            let mut value = 0i64;
            for &byte in bytes.get(from..from + count).ok_or_else(malformed)? {
                if !byte.is_ascii_digit() {
                    return Err(malformed());
                }
                value = value * 10 + i64::from(byte - b'0');
            }
            Ok(value)
        };
        let expect = |at: usize, byte: u8| -> Result<(), TimeError> {
            if bytes.get(at) == Some(&byte) {
                Ok(())
            } else {
                Err(malformed())
            }
        };

        let year = digits(0, 4)?;
        expect(4, b'-')?;
        let month = digits(5, 2)?;
        expect(7, b'-')?;
        let day = digits(8, 2)?;
        expect(10, b'T')?;
        let hour = digits(11, 2)?;
        expect(13, b':')?;
        let minute = digits(14, 2)?;
        expect(16, b':')?;
        let second = digits(17, 2)?;

        let mut at = 19;
        let mut nanos = 0u32;
        if bytes.get(at) == Some(&b'.') {
            at += 1;
            let start = at;
            while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            let count = at - start;
            if count == 0 || count > 9 {
                return Err(malformed());
            }
            let fraction = digits(start, count)?;
            let scale = 10i64.pow(9 - count as u32);
            nanos = u32::try_from(fraction * scale).map_err(|_| malformed())?;
        }

        // Seconds east of UTC. A time written with +01:00 is an hour ahead
        // of UTC, so the offset is subtracted.
        let offset_secs = match bytes.get(at) {
            Some(b'Z') if at + 1 == bytes.len() => 0,
            Some(sign @ (b'+' | b'-')) if at + 6 == bytes.len() => {
                let hours = digits(at + 1, 2)?;
                expect(at + 3, b':')?;
                let minutes = digits(at + 4, 2)?;
                if hours > 23 {
                    return Err(range("the offset's hours"));
                }
                if minutes > 59 {
                    return Err(range("the offset's minutes"));
                }
                let offset = hours * 3600 + minutes * 60;
                if *sign == b'+' { offset } else { -offset }
            }
            _ => return Err(malformed()),
        };

        if !(1..=12).contains(&month) {
            return Err(range("the month"));
        }
        if day < 1 || day > days_in_month(year, month) {
            return Err(range("the day"));
        }
        if hour > 23 {
            return Err(range("the hour"));
        }
        if minute > 59 {
            return Err(range("the minute"));
        }
        if second > 59 {
            return Err(range("the second"));
        }

        let secs =
            days_from_civil(year, month, day) * SECS_PER_DAY + hour * 3600 + minute * 60 + second
                - offset_secs;
        Self::from_unix(secs, nanos).ok_or_else(|| range("the year, once in UTC,"))
    }
}

impl std::fmt::Display for UtcTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let days = self.secs.div_euclid(SECS_PER_DAY);
        let in_day = self.secs.rem_euclid(SECS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let (hour, minute, second) = (in_day / 3600, in_day % 3600 / 60, in_day % 60);
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
        )?;
        if self.nanos != 0 {
            if self.nanos.is_multiple_of(1_000_000) {
                write!(f, ".{:03}", self.nanos / 1_000_000)?;
            } else if self.nanos.is_multiple_of(1_000) {
                write!(f, ".{:06}", self.nanos / 1_000)?;
            } else {
                write!(f, ".{:09}", self.nanos)?;
            }
        }
        f.write_str("Z")
    }
}

impl std::str::FromStr for UtcTime {
    type Err = TimeError;

    fn from_str(text: &str) -> Result<Self, TimeError> {
        Self::parse(text)
    }
}

impl TryFrom<&Timestamp> for UtcTime {
    type Error = TimeError;

    fn try_from(timestamp: &Timestamp) -> Result<Self, TimeError> {
        Self::parse(timestamp.as_str())
    }
}

impl From<UtcTime> for Timestamp {
    fn from(time: UtcTime) -> Self {
        time.to_timestamp()
    }
}

const fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the given date of the proleptic Gregorian
/// calendar. The year is counted from March, so that the leap day is the
/// last day of the counted year, and years are grouped into the calendar's
/// 400-year cycle of 146,097 days.
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let counted_year = if month <= 2 { year - 1 } else { year };
    let cycle = counted_year.div_euclid(400);
    let year_of_cycle = counted_year.rem_euclid(400);
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_cycle = year_of_cycle * 365 + year_of_cycle / 4 - year_of_cycle / 100 + day_of_year;
    // 719,468 days separate 0000-03-01 from 1970-01-01.
    cycle * 146_097 + day_of_cycle - 719_468
}

/// The date `days` after 1970-01-01: the inverse of [`days_from_civil`].
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let cycle = shifted.div_euclid(146_097);
    let day_of_cycle = shifted.rem_euclid(146_097);
    let year_of_cycle = (day_of_cycle - day_of_cycle / 1_460 + day_of_cycle / 36_524
        - day_of_cycle / 146_096)
        / 365;
    let day_of_year =
        day_of_cycle - (365 * year_of_cycle + year_of_cycle / 4 - year_of_cycle / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_cycle + cycle * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::{self, ensure};
    use crate::rng::Rng;

    fn time(text: &str) -> UtcTime {
        UtcTime::parse(text).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn known_moments_are_written_as_the_protocol_writes_them() {
        let cases = [
            (0, 0, "1970-01-01T00:00:00Z"),
            (1_794_478_502, 0, "2026-11-12T10:15:02Z"),
            (1_794_478_502, 250_000_000, "2026-11-12T10:15:02.250Z"),
            (1_794_478_502, 250_001_000, "2026-11-12T10:15:02.250001Z"),
            (1_794_478_502, 1, "2026-11-12T10:15:02.000000001Z"),
            (951_782_400, 0, "2000-02-29T00:00:00Z"),
            (-1, 0, "1969-12-31T23:59:59Z"),
            (-1, 999_000_000, "1969-12-31T23:59:59.999Z"),
            (MIN_SECS, 0, "0000-01-01T00:00:00Z"),
            (MAX_SECS, 999_999_999, "9999-12-31T23:59:59.999999999Z"),
        ];
        for (secs, nanos, text) in cases {
            let moment = UtcTime::from_unix(secs, nanos).unwrap();
            assert_eq!(moment.to_string(), text);
            assert_eq!(moment.to_timestamp().as_str(), text);
            assert_eq!(time(text), moment, "{text} reads back");
            assert_eq!(UtcTime::try_from(&Timestamp::new(text)), Ok(moment));
        }
    }

    #[test]
    fn an_offset_is_converted_to_utc() {
        let utc = time("2026-11-12T10:15:02Z");
        assert_eq!(time("2026-11-12T11:15:02+01:00"), utc);
        assert_eq!(time("2026-11-12T02:15:02-08:00"), utc);
        assert_eq!(time("2026-11-12T15:45:02+05:30"), utc);
        assert_eq!(time("2026-11-12T10:15:02+00:00"), utc);
        assert_eq!(time("2026-11-12T10:15:02-00:00"), utc);
        assert_eq!(time("2026-11-13T00:15:02+14:00"), utc);
        assert_eq!(time("2026-11-12T10:15:02.5Z").subsec_nanos(), 500_000_000);
    }

    #[test]
    fn anything_else_is_refused() {
        let malformed = [
            "",
            "2026-11-12",
            "2026-11-12T10:15:02",
            "2026-11-12t10:15:02Z",
            "2026-11-12T10:15:02z",
            "2026-11-12 10:15:02Z",
            "2026-11-12T10:15:02.Z",
            "2026-11-12T10:15:02.1234567890Z",
            "2026-11-12T10:15:02+0100",
            "2026-11-12T10:15:02+01",
            "2026-11-12T10:15:02Z ",
            " 2026-11-12T10:15:02Z",
            "2026-11-12T10:15:02ZZ",
            "2026-11-12T10:15Z0000000",
            "26-11-12T10:15:02Z00",
            "2026-11-12T10:15:02+01:00:00",
            "２０２６-11-12T10:15:02Z",
            "+026-11-12T10:15:02Z",
            "2026-11-12T10:15:-2Z",
        ];
        for text in malformed {
            assert!(
                matches!(UtcTime::parse(text), Err(TimeError::Malformed(_))),
                "{text:?} should be malformed, got {:?}",
                UtcTime::parse(text)
            );
        }
        let out_of_range = [
            "2026-00-12T10:15:02Z",
            "2026-13-12T10:15:02Z",
            "2026-11-00T10:15:02Z",
            "2026-11-31T10:15:02Z",
            "2026-02-29T10:15:02Z",
            "1900-02-29T10:15:02Z",
            "2026-11-12T24:00:00Z",
            "2026-11-12T10:60:02Z",
            "2026-12-31T23:59:60Z",
            "2026-11-12T10:15:02+24:00",
            "2026-11-12T10:15:02+01:60",
            "0000-01-01T00:00:00+00:01",
            "9999-12-31T23:59:59-00:01",
        ];
        for text in out_of_range {
            assert!(
                matches!(UtcTime::parse(text), Err(TimeError::OutOfRange(..))),
                "{text:?} should be out of range, got {:?}",
                UtcTime::parse(text)
            );
        }
        let error = UtcTime::parse("2026-13-12T10:15:02Z").unwrap_err();
        assert!(error.to_string().contains("the month"), "{error}");
    }

    #[test]
    fn times_outside_four_digit_years_do_not_exist() {
        assert_eq!(UtcTime::from_unix(MIN_SECS - 1, 0), None);
        assert_eq!(UtcTime::from_unix(MAX_SECS + 1, 0), None);
        assert_eq!(UtcTime::from_unix(0, NANOS_PER_SEC), None);
        let last = UtcTime::from_unix(MAX_SECS, 0).unwrap();
        assert_eq!(last.checked_add(Duration::from_secs(1)), None);
        assert_eq!(
            last.checked_add(Duration::from_nanos(999_999_999))
                .map(UtcTime::unix_secs),
            Some(MAX_SECS)
        );
        let first = UtcTime::from_unix(MIN_SECS, 0).unwrap();
        assert_eq!(first.checked_sub(Duration::from_nanos(1)), None);
        assert_eq!(UtcTime::UNIX_EPOCH.checked_add(Duration::MAX), None);
        assert_eq!(UtcTime::UNIX_EPOCH.checked_sub(Duration::MAX), None);
    }

    #[test]
    fn arithmetic_carries_across_the_second() {
        let start = time("2026-11-12T10:15:02.750Z");
        let later = start.checked_add(Duration::from_millis(500)).unwrap();
        assert_eq!(later.to_string(), "2026-11-12T10:15:03.250Z");
        assert_eq!(
            later.duration_since(start),
            Some(Duration::from_millis(500))
        );
        assert_eq!(start.duration_since(later), None, "the clock went back");
        assert_eq!(later.checked_sub(Duration::from_millis(500)), Some(start));
        assert_eq!(start.duration_since(start), Some(Duration::ZERO));
        assert_eq!(start.unix_millis(), 1_794_478_502_750);
        assert_eq!(UtcTime::from_unix_millis(1_794_478_502_750), Some(start));
        assert_eq!(
            UtcTime::from_unix_millis(-1).unwrap().to_string(),
            "1969-12-31T23:59:59.999Z"
        );
        assert_eq!(UtcTime::from_unix_millis(-1).unwrap().unix_millis(), -1);
    }

    #[test]
    fn the_systems_time_converts_on_both_sides_of_the_epoch() {
        // In steps of 100 ns, which is as fine as Windows keeps a time: a
        // `SystemTime` there cannot hold 5 ns, and gives back 0.
        let after = SystemTime::UNIX_EPOCH + Duration::new(1_794_478_502, 500);
        assert_eq!(
            UtcTime::from_system(after),
            UtcTime::from_unix(1_794_478_502, 500)
        );
        // Not every platform can represent a time before 1970.
        if let Some(before) = SystemTime::UNIX_EPOCH.checked_sub(Duration::new(1, 250_000_000)) {
            assert_eq!(
                UtcTime::from_system(before),
                UtcTime::from_unix(-2, 750_000_000)
            );
        }
        if let Some(before) = SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(3)) {
            assert_eq!(UtcTime::from_system(before), UtcTime::from_unix(-3, 0));
        }
    }

    #[test]
    fn every_day_of_the_calendar_converts_both_ways() {
        // Every day from 0000-01-01 to 9999-12-31, walked in order: the day
        // number must rise by one, and the date must follow the calendar.
        //
        // That is 3.6 million days. On a pull request the walk is of the
        // eight centuries from 1600 to 2399, which is two whole cycles of
        // the calendar's rules and every date the harness will meet, and
        // the two ends of the range are checked by themselves. The nightly
        // run walks all of it.
        assert_eq!(days_from_civil(0, 1, 1) * SECS_PER_DAY, MIN_SECS);
        assert_eq!(civil_from_days(days_from_civil(0, 1, 1)), (0, 1, 1));
        assert_eq!(
            (days_from_civil(9999, 12, 31) + 1) * SECS_PER_DAY - 1,
            MAX_SECS
        );
        assert_eq!(
            civil_from_days(days_from_civil(9999, 12, 31)),
            (9999, 12, 31)
        );
        let (first, last) = if cases::exhaustive() {
            (0i64, 9999i64)
        } else {
            (1600, 2399)
        };
        let (mut year, mut month, mut day) = (first, 1i64, 1i64);
        let mut number = days_from_civil(first, 1, 1);
        loop {
            assert_eq!(days_from_civil(year, month, day), number);
            assert_eq!(civil_from_days(number), (year, month, day));
            day += 1;
            if day > days_in_month(year, month) {
                day = 1;
                month += 1;
                if month > 12 {
                    month = 1;
                    year += 1;
                    if year > last {
                        break;
                    }
                }
            }
            number += 1;
        }
        assert_eq!(number, days_from_civil(last, 12, 31));
    }

    #[test]
    fn property_a_time_survives_writing_and_reading() {
        cases::check(
            "time_round_trip",
            2000,
            |rng| {
                let secs = MIN_SECS + rng.below((MAX_SECS - MIN_SECS + 1) as u64) as i64;
                let nanos = match rng.below(4) {
                    0 => 0,
                    1 => rng.below(1000) as u32 * 1_000_000,
                    2 => rng.below(1_000_000) as u32 * 1_000,
                    _ => rng.below(u64::from(NANOS_PER_SEC)) as u32,
                };
                (secs, nanos)
            },
            |&(secs, nanos)| {
                let moment = UtcTime::from_unix(secs, nanos).ok_or("out of range")?;
                let text = moment.to_string();
                let read = UtcTime::parse(&text).map_err(|error| error.to_string())?;
                ensure!(read == moment, "{text} read back as {read}");
                ensure!(
                    text.len() == 20 || [24, 27, 30].contains(&text.len()),
                    "{text} has an unexpected length"
                );
                Ok(())
            },
        );
    }

    #[test]
    fn property_text_order_is_time_order() {
        cases::check(
            "time_order",
            2000,
            |rng| {
                let pick = |rng: &mut crate::SeededRng| {
                    MIN_SECS + rng.below((MAX_SECS - MIN_SECS + 1) as u64) as i64
                };
                (pick(rng), pick(rng))
            },
            |&(a, b)| {
                let (first, second) = (
                    UtcTime::from_unix(a, 0).ok_or("out of range")?,
                    UtcTime::from_unix(b, 0).ok_or("out of range")?,
                );
                ensure!(
                    first.cmp(&second) == first.to_string().cmp(&second.to_string()),
                    "{first} and {second} sort differently as text"
                );
                Ok(())
            },
        );
    }
}
