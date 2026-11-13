//! A check that a secret never reaches a printed form of a value
//! (`docs/security.md`, "Secrets in logs and session files").
//!
//! A test puts a made-up key (the canary) everywhere a type can hold one,
//! then hands the values to [`assert_never_printed`], which formats each
//! one every way a log line or an error message could (`{:?}`, `{:#?}`,
//! `{}` and `{:#}`) and fails if the canary shows in any of them.
//!
//! ```ignore
//! let options = LoadOptions::new(env_with_the_canary);
//! assert_never_printed(CANARY, &[
//!     Printed::debug("LoadOptions", &options),
//!     Printed::both("CredentialError", &error),
//! ]);
//! ```
//!
//! Run it over whole values, not over the fields known to hold a key today:
//! a field added later is then checked by the same test, and cannot bring a
//! leak back unnoticed.

use std::fmt::{Debug, Display};

/// The shortest canary accepted. A short one could turn up in printed text
/// by chance and fail a test for no reason.
const SHORTEST: usize = 12;

/// The printed forms of one value, under a name for the failure message.
/// It has no `Debug` of its own: the forms it holds may hold the canary.
#[derive(Clone)]
#[expect(
    missing_debug_implementations,
    reason = "what it holds is what is being checked for a leak"
)]
pub struct Printed {
    name: String,
    forms: Vec<(&'static str, String)>,
}

impl Printed {
    /// The value's `Debug` forms: `{:?}` and `{:#?}`.
    #[must_use]
    pub fn debug(name: impl Into<String>, value: &(impl Debug + ?Sized)) -> Self {
        Self {
            name: name.into(),
            forms: vec![
                ("{:?}", format!("{value:?}")),
                ("{:#?}", format!("{value:#?}")),
            ],
        }
    }

    /// The value's `Display` forms: `{}` and `{:#}`.
    #[must_use]
    pub fn display(name: impl Into<String>, value: &(impl Display + ?Sized)) -> Self {
        Self {
            name: name.into(),
            forms: vec![("{}", format!("{value}")), ("{:#}", format!("{value:#}"))],
        }
    }

    /// Both: the `Debug` and the `Display` forms.
    #[must_use]
    pub fn both(name: impl Into<String>, value: &(impl Debug + Display + ?Sized)) -> Self {
        let mut printed = Self::debug(name, value);
        printed.forms.extend(Self::display("", value).forms);
        printed
    }
}

/// Fails the test if `canary` appears in any printed form of any value.
///
/// The failure message names each value and form that holds the canary,
/// and shows the form with the canary replaced by `<canary>`, so that the
/// message points at the leak without repeating the key.
///
/// # Panics
///
/// When the canary is printed, when it is shorter than 12 characters, and
/// when no value is given.
#[track_caller]
pub fn assert_never_printed(canary: &str, printed: &[Printed]) {
    assert!(
        canary.chars().count() >= SHORTEST,
        "a canary must be at least {SHORTEST} characters, so that it cannot turn up by chance"
    );
    assert!(!printed.is_empty(), "no value was given to check");
    let leaks: Vec<String> = printed
        .iter()
        .flat_map(|value| {
            value
                .forms
                .iter()
                .filter(|(_, text)| text.contains(canary))
                .map(|(form, text)| {
                    format!(
                        "{} printed with {form}: {}",
                        value.name,
                        text.replace(canary, "<canary>")
                    )
                })
        })
        .collect();
    assert!(
        leaks.is_empty(),
        "the canary was printed:\n{}",
        leaks.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;

    const CANARY: &str = "sk-test-canary-0001";

    /// Prints the key in its `Display` form only.
    struct Careless(&'static str);

    impl Debug for Careless {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("Careless(..)")
        }
    }

    impl Display for Careless {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "the key is {}", self.0)
        }
    }

    #[test]
    fn values_that_hold_no_canary_pass() {
        assert_never_printed(
            CANARY,
            &[
                Printed::debug("a number", &5),
                Printed::both("a word", "nothing to see"),
                Printed::display("redacted", &cortex_base::Secret::new(CANARY)),
            ],
        );
    }

    #[test]
    fn a_canary_in_any_form_fails_and_is_not_repeated() {
        let fails = |printed: Vec<Printed>| {
            std::panic::catch_unwind(|| assert_never_printed(CANARY, &printed))
                .expect_err("the canary was printed")
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default()
        };
        let message = fails(vec![Printed::debug("plain", &CANARY)]);
        assert!(message.contains("plain printed with {:?}"), "{message}");
        assert!(message.contains("plain printed with {:#?}"), "{message}");
        assert!(!message.contains(CANARY), "{message}");

        // The Debug form is careful and the Display form is not: `both`
        // looks at each.
        let careless = Careless(CANARY);
        assert_never_printed(CANARY, &[Printed::debug("careless", &careless)]);
        let message = fails(vec![Printed::both("careless", &careless)]);
        assert!(
            message.contains("careless printed with {}: the key is <canary>"),
            "{message}"
        );
    }

    #[test]
    fn a_short_canary_or_nothing_to_check_is_a_mistake_in_the_test() {
        assert!(
            std::panic::catch_unwind(|| assert_never_printed("sk-1", &[Printed::debug("x", &1)]))
                .is_err()
        );
        assert!(std::panic::catch_unwind(|| assert_never_printed(CANARY, &[])).is_err());
    }
}
