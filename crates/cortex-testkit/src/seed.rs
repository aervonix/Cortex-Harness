//! The seed of a test whose order of events is seeded
//! (`docs/testing-and-ci.md`, "A seeded order of events").
//!
//! A test rig that would otherwise see events in the order the machine
//! happened to produce them takes that order from a seeded generator, and
//! takes the seed from here:
//!
//! - an ordinary run uses seed 0, every time, so a test passes or fails the
//!   same way on every machine;
//! - `cargo xtask stress` runs each test many times, and each repeat gets
//!   the next seed, so a test that depends on one order fails on some;
//! - a failed test's output has the line this module printed, with its
//!   seed, and `CORTEX_TEST_SEED=<seed>` runs the test again in that order.

/// The variable that fixes the seed of every seeded test in a run.
pub const SEED: &str = "CORTEX_TEST_SEED";

/// The variable `cargo xtask stress` sets to the seed of the first repeat.
/// Repeat `n`, counted from 0, runs with this plus `n`.
pub const FIRST_SEED: &str = "CORTEX_TEST_FIRST_SEED";

/// The variable nextest sets to the number of the repeat, counted from 0,
/// in a `--stress-count` run. In any other run it sets it to `none`.
const REPEAT: &str = "NEXTEST_STRESS_CURRENT";

/// The seed for the test that calls this, and a line on standard output
/// that says what it is. The test runner shows a test's output when the
/// test fails, so a failure always comes with the seed that reproduces it.
///
/// # Panics
///
/// When one of the variables is set to something that is not a number: a
/// run that was asked for one order must not quietly use another.
#[must_use]
#[allow(
    clippy::print_stdout,
    reason = "the line is for the output the test runner keeps for a failed test"
)]
pub fn for_this_test() -> u64 {
    let seed = seed_from(|name| std::env::var(name).ok());
    println!("test seed {seed}: run this test again in the same order with {SEED}={seed}");
    seed
}

/// The seed the variables ask for.
fn seed_from(variable: impl Fn(&str) -> Option<String>) -> u64 {
    let number = |name: &str| {
        variable(name).map(|text| {
            text.trim()
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("{name}={text} is not a whole number"))
        })
    };
    // The repeat is nextest's to set, and what is not a number there means
    // "this is not a stress run".
    let repeat = variable(REPEAT)
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    match number(SEED) {
        Some(seed) => seed,
        None => number(FIRST_SEED).unwrap_or(0).wrapping_add(repeat),
    }
}

#[cfg(test)]
mod tests {
    use super::seed_from;

    fn with(variables: &[(&str, &str)]) -> u64 {
        seed_from(|name| {
            variables
                .iter()
                .find(|(variable, _)| *variable == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn an_ordinary_run_has_seed_zero() {
        assert_eq!(with(&[]), 0);
        // What nextest sets when the run is not a stress run.
        assert_eq!(with(&[("NEXTEST_STRESS_CURRENT", "none")]), 0);
    }

    #[test]
    fn each_repeat_of_a_stress_run_has_the_next_seed() {
        assert_eq!(with(&[("NEXTEST_STRESS_CURRENT", "0")]), 0);
        assert_eq!(with(&[("NEXTEST_STRESS_CURRENT", "41")]), 41);
        assert_eq!(
            with(&[
                ("CORTEX_TEST_FIRST_SEED", "1000"),
                ("NEXTEST_STRESS_CURRENT", "41")
            ]),
            1041
        );
        assert_eq!(with(&[("CORTEX_TEST_FIRST_SEED", "1000")]), 1000);
    }

    #[test]
    fn a_seed_that_is_asked_for_is_the_seed_whatever_the_repeat() {
        assert_eq!(
            with(&[
                ("CORTEX_TEST_SEED", "1041"),
                ("CORTEX_TEST_FIRST_SEED", "5"),
                ("NEXTEST_STRESS_CURRENT", "3")
            ]),
            1041
        );
        assert_eq!(with(&[("CORTEX_TEST_SEED", " 7\n")]), 7);
    }

    #[test]
    #[should_panic(expected = "CORTEX_TEST_SEED=seven is not a whole number")]
    fn a_seed_that_is_not_a_number_is_refused() {
        let _ = with(&[("CORTEX_TEST_SEED", "seven")]);
    }

    #[test]
    #[should_panic(expected = "CORTEX_TEST_FIRST_SEED=-1 is not a whole number")]
    fn a_first_seed_that_is_not_a_number_is_refused() {
        let _ = with(&[("CORTEX_TEST_FIRST_SEED", "-1")]);
    }
}
