//! Whether a request begins with the one before it
//! (`docs/architecture/04-providers.md`, "Prompt caching").
//!
//! Two checks, at two depths:
//!
//! - **In bytes**, in a debug build: each request's stable prefix, as its
//!   protocol writes it ([`cortex_llm::Protocol::stable_prefix`]), must
//!   begin with the stable prefix of the request before it in the same
//!   epoch, sent with the same settings. A break is a bug in the context:
//!   the loop stops the turn with a panic that says where the bytes first
//!   differ. [`check_prefix`] is the comparison, which the tests use too.
//! - **In what was assembled**, always, for the context record: where the
//!   request first differs from the one before it, whatever the epoch
//!   ([`PrefixDifference`]). On a cache miss it says what moved.

use std::hash::{DefaultHasher, Hash, Hasher};

/// Where a request first differs from the request before it in the
/// session, as the context record says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrefixDifference {
    /// It is the session's first request in this process.
    First,
    /// It begins with the whole of the request before it.
    Extends,
    /// It goes to another model, or asks for another speed, reasoning
    /// level or output limit: a provider's cache is kept for each.
    Settings,
    /// Its system prompt is another.
    System,
    /// Its tools are others, or in another order.
    Tools,
    /// Its messages differ from the one at this index on.
    Message(usize),
}

/// What a request was, as far as [`difference`] compares it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) settings: u64,
    pub(crate) system: u64,
    pub(crate) tools: u64,
    pub(crate) messages: Vec<u64>,
}

/// A hash of anything that can be hashed, for a [`Shape`].
pub(crate) fn hash_of(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Where `now` first differs from `previous`.
pub(crate) fn difference(previous: Option<&Shape>, now: &Shape) -> PrefixDifference {
    let Some(previous) = previous else {
        return PrefixDifference::First;
    };
    if previous.settings != now.settings {
        return PrefixDifference::Settings;
    }
    if previous.tools != now.tools {
        return PrefixDifference::Tools;
    }
    if previous.system != now.system {
        return PrefixDifference::System;
    }
    match previous
        .messages
        .iter()
        .zip(&now.messages)
        .position(|(earlier, later)| earlier != later)
    {
        Some(index) => PrefixDifference::Message(index),
        None if now.messages.len() < previous.messages.len() => {
            PrefixDifference::Message(now.messages.len())
        }
        None => PrefixDifference::Extends,
    }
}

/// Whether `earlier` is the beginning of `later`: `Err` with the offset of
/// the first byte at which they part when it is not.
///
/// # Errors
///
/// The offset of the first byte that differs, or the end of `later` when
/// it is shorter.
pub fn check_prefix(earlier: &[u8], later: &[u8]) -> Result<(), usize> {
    match earlier
        .iter()
        .zip(later)
        .position(|(one, other)| one != other)
    {
        Some(at) => Err(at),
        None if later.len() < earlier.len() => Err(later.len()),
        None => Ok(()),
    }
}

/// What the bytes around `at` are, for a person reading the failure.
pub(crate) fn excerpt(bytes: &[u8], at: usize) -> String {
    let from = at.saturating_sub(80);
    let to = (at + 80).min(bytes.len());
    String::from_utf8_lossy(&bytes[from.min(to)..to]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_is_found_and_a_change_is_placed() {
        assert_eq!(check_prefix(b"{\"a\":[1", b"{\"a\":[1,2"), Ok(()));
        assert_eq!(check_prefix(b"{\"a\":[1", b"{\"a\":[3,2"), Err(6));
        assert_eq!(check_prefix(b"{\"a\":[1,2", b"{\"a\":[1"), Err(7));
    }

    #[test]
    fn the_difference_names_the_first_thing_that_moved() {
        let base = Shape {
            settings: 1,
            system: 2,
            tools: 3,
            messages: vec![10, 11],
        };
        assert_eq!(difference(None, &base), PrefixDifference::First);
        let grown = Shape {
            messages: vec![10, 11, 12],
            ..base.clone()
        };
        assert_eq!(difference(Some(&base), &grown), PrefixDifference::Extends);
        let edited = Shape {
            messages: vec![10, 99, 12],
            ..base.clone()
        };
        assert_eq!(
            difference(Some(&base), &edited),
            PrefixDifference::Message(1)
        );
        let system = Shape {
            system: 9,
            ..grown.clone()
        };
        assert_eq!(difference(Some(&base), &system), PrefixDifference::System);
        let model = Shape {
            settings: 9,
            ..grown
        };
        assert_eq!(difference(Some(&base), &model), PrefixDifference::Settings);
        let shorter = Shape {
            messages: vec![10],
            ..base.clone()
        };
        assert_eq!(
            difference(Some(&base), &shorter),
            PrefixDifference::Message(1)
        );
    }
}
