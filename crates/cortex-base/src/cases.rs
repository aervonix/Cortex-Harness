//! Property tests without a property-testing crate
//! (`docs/testing-and-ci.md`, "Property tests").
//!
//! A property is checked against many generated inputs. Each input comes
//! from a [`SeededRng`] whose seed is derived from the property's name and
//! the case number, so a run is the same on every machine, and one failing
//! case can be run again alone.
//!
//! - A failure prints the property, the seed, the case number and the input.
//! - `CORTEX_PROPERTY_SCALE=50` multiplies every property's number of cases.
//!   The blocking tier runs at 1; a nightly run sets it higher.
//! - `CORTEX_PROPERTY_SEED=<seed>` runs only the case with that seed: the one
//!   a failure printed.
//!
//! There is no shrinking. Generators therefore prefer small inputs, so that
//! a failing case is short enough to read.

use std::fmt::Debug;

use crate::rng::{Rng, SeededRng};

/// The environment variable that multiplies the number of cases.
const SCALE: &str = "CORTEX_PROPERTY_SCALE";
/// The environment variable that names one seed to run.
const SEED: &str = "CORTEX_PROPERTY_SEED";

/// Returns an error from a property unless the condition holds.
macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            return Err(format!($($message)+).into());
        }
    };
}
pub(crate) use ensure;

/// What a property returns: nothing, or what was wrong with the input.
pub(crate) type Outcome = Result<(), Box<dyn std::error::Error>>;

/// Checks `property` against `cases` inputs made by `generate`. Panics, with
/// what is needed to reproduce it, on the first input that fails.
pub(crate) fn check<T: Debug>(
    name: &str,
    cases: u64,
    generate: impl Fn(&mut SeededRng) -> T,
    property: impl Fn(&T) -> Outcome,
) {
    let run = |seed: u64, case: Option<u64>| {
        let input = generate(&mut SeededRng::new(seed));
        if let Err(problem) = property(&input) {
            let case = case.map_or_else(String::new, |case| format!(", case {case}"));
            panic!(
                "property `{name}` failed{case}\n  seed:  {seed}  (run it alone with \
                 {SEED}={seed})\n  input: {input:?}\n  {problem}"
            );
        }
    };

    if let Some(seed) = number_from_env(SEED) {
        run(seed, None);
        return;
    }
    let scale = number_from_env(SCALE).unwrap_or(1).max(1);
    let base = seed_of(name);
    for case in 0..cases.saturating_mul(scale) {
        // One step of a generator seeded by the name and the case number
        // gives every case a seed of its own that can be run alone.
        let seed = SeededRng::new(base ^ case).next_u64();
        run(seed, Some(case));
    }
}

/// A seed from a property's name: FNV-1a, so that it is the same everywhere.
fn seed_of(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn number_from_env(variable: &str) -> Option<u64> {
    let value = std::env::var(variable).ok()?;
    match value.trim().parse() {
        Ok(number) => Some(number),
        Err(_) => panic!("{variable} must be a whole number, and is `{value}`"),
    }
}

/// Picks one element of a non-empty slice.
pub(crate) fn pick<'a, T>(rng: &mut SeededRng, items: &'a [T]) -> &'a T {
    &items[rng.below(items.len() as u64) as usize]
}

/// A length that is usually small: half the time under 4, rarely near `max`.
pub(crate) fn small(rng: &mut SeededRng, max: u64) -> usize {
    let bound = match rng.below(8) {
        0..=3 => 4,
        4..=6 => 16,
        _ => max,
    };
    rng.below(bound.clamp(1, max.max(1)) + 1).min(max) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passing_property_runs_every_case() {
        let count = std::cell::Cell::new(0u64);
        check(
            "counts",
            50,
            |rng| rng.below(10),
            |value| {
                count.set(count.get() + 1);
                ensure!(*value < 10, "{value} is not below 10");
                Ok(())
            },
        );
        let scale = number_from_env(SCALE).unwrap_or(1).max(1);
        if number_from_env(SEED).is_none() {
            assert_eq!(count.get(), 50 * scale);
        }
    }

    #[test]
    fn a_failure_names_the_seed_and_the_input() {
        let outcome = std::panic::catch_unwind(|| {
            check(
                "always_fails",
                10,
                |rng| rng.below(1000),
                |value| {
                    ensure!(*value > 1000, "{value} is too small");
                    Ok(())
                },
            );
        });
        let panic = outcome.expect_err("the property fails");
        let message = panic.downcast_ref::<String>().expect("a formatted message");
        assert!(
            message.contains("property `always_fails` failed"),
            "{message}"
        );
        assert!(message.contains("seed:"), "{message}");
        assert!(message.contains("CORTEX_PROPERTY_SEED="), "{message}");
        assert!(message.contains("input:"), "{message}");
        assert!(message.contains("is too small"), "{message}");
    }

    #[test]
    fn the_same_name_gives_the_same_inputs() {
        let collect = || {
            let seen = std::cell::RefCell::new(Vec::new());
            check(
                "repeatable",
                20,
                |rng| rng.next_u64(),
                |value| {
                    seen.borrow_mut().push(*value);
                    Ok(())
                },
            );
            seen.into_inner()
        };
        let first = collect();
        assert_eq!(first, collect());
        if number_from_env(SEED).is_none() {
            let distinct: std::collections::BTreeSet<_> = first.iter().collect();
            assert_eq!(distinct.len(), first.len(), "every case has its own seed");
        }
    }

    #[test]
    fn small_lengths_stay_in_range() {
        let mut rng = SeededRng::new(7);
        for max in [0, 1, 3, 40] {
            for _ in 0..200 {
                assert!(small(&mut rng, max) as u64 <= max);
            }
        }
        assert_eq!(*pick(&mut rng, &[5]), 5);
    }
}
