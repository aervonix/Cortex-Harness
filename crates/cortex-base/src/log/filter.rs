//! Which log lines are wanted: a level, and levels of their own for the
//! targets that begin with a given prefix
//! (`docs/architecture/02-runtime.md`, "Levels").
//!
//! The text form is what `--log-level`, `CORTEX_LOG` and `log.level` take:
//! `info`, or `info,cortex_llm=debug,cortex_tools::shell=trace`. It is a
//! prefix match on the target, the longest prefix winning, and nothing
//! more: no regular expressions and no spans in the filter.

use tracing::Level;
use tracing::metadata::LevelFilter;

/// The level a filter has when nothing sets one.
pub const DEFAULT_LEVEL: LevelFilter = LevelFilter::INFO;

/// A filter's text that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{part}` is not a log level. Use `off`, `error`, `warn`, `info`, `debug` or `trace`, \
     alone or as `target=level`, with commas between."
)]
pub struct FilterError {
    /// The part that was not understood.
    pub part: String,
}

/// A level for everything, and levels for the targets under a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    default: LevelFilter,
    /// Longest prefix first, so that the first match is the closest.
    overrides: Vec<(String, LevelFilter)>,
}

impl Default for Filter {
    fn default() -> Self {
        Self::at(DEFAULT_LEVEL)
    }
}

impl Filter {
    /// A filter that lets through `level` and everything more serious,
    /// whatever the target.
    #[must_use]
    pub fn at(level: LevelFilter) -> Self {
        Self {
            default: level,
            overrides: Vec::new(),
        }
    }

    /// Reads a filter: a level, `target=level` pairs, or both, with commas
    /// between. Empty text is the default level. A level named twice for
    /// the same thing takes the last.
    ///
    /// # Errors
    ///
    /// When a part names no level.
    pub fn parse(text: &str) -> Result<Self, FilterError> {
        let mut filter = Self::default();
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let error = || FilterError {
                part: part.to_owned(),
            };
            match part.split_once('=') {
                None => filter.default = level(part).ok_or_else(error)?,
                Some((target, name)) => {
                    let target = target.trim();
                    if target.is_empty() {
                        return Err(error());
                    }
                    let level = level(name.trim()).ok_or_else(error)?;
                    filter = filter.with_target(target, level);
                }
            }
        }
        Ok(filter)
    }

    /// The same filter with `level` for every target that begins with
    /// `prefix`.
    #[must_use]
    pub fn with_target(mut self, prefix: impl Into<String>, level: LevelFilter) -> Self {
        let prefix = prefix.into();
        self.overrides.retain(|(known, _)| *known != prefix);
        self.overrides.push((prefix, level));
        self.overrides
            .sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        self
    }

    /// The level that applies to `target`.
    #[must_use]
    pub fn level_of(&self, target: &str) -> LevelFilter {
        self.overrides
            .iter()
            .find(|(prefix, _)| target.starts_with(prefix.as_str()))
            .map_or(self.default, |(_, level)| *level)
    }

    /// Whether a line of `level` from `target` is wanted.
    #[must_use]
    pub fn enabled(&self, level: &Level, target: &str) -> bool {
        self.level_of(target) >= *level
    }

    /// The most detailed level any target is given: nothing more detailed
    /// than this is ever wanted, so a call site beyond it costs one
    /// comparison.
    #[must_use]
    pub fn most_detailed(&self) -> LevelFilter {
        self.overrides
            .iter()
            .map(|(_, level)| *level)
            .fold(self.default, std::cmp::max)
    }
}

/// The level a name stands for.
fn level(name: &str) -> Option<LevelFilter> {
    Some(match name.to_ascii_lowercase().as_str() {
        "off" => LevelFilter::OFF,
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "info" => LevelFilter::INFO,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        _ => return None,
    })
}

/// A level's name, as the log writes it.
#[must_use]
pub fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_alone_is_for_everything() {
        let filter = Filter::parse("warn").unwrap();
        assert!(filter.enabled(&Level::ERROR, "cortex_llm"));
        assert!(filter.enabled(&Level::WARN, "anything::at::all"));
        assert!(!filter.enabled(&Level::INFO, "cortex_llm"));
        assert_eq!(filter.most_detailed(), LevelFilter::WARN);
    }

    #[test]
    fn empty_text_is_the_default() {
        for text in ["", " ", ",", " , "] {
            assert_eq!(Filter::parse(text).unwrap(), Filter::default(), "{text:?}");
        }
        assert!(Filter::default().enabled(&Level::INFO, "x"));
        assert!(!Filter::default().enabled(&Level::DEBUG, "x"));
    }

    #[test]
    fn a_target_prefix_has_its_own_level_and_the_longest_wins() {
        let filter =
            Filter::parse("info, cortex_llm=debug ,cortex_llm::sse=trace,cortex_tools=off")
                .unwrap();
        assert!(filter.enabled(&Level::DEBUG, "cortex_llm"));
        assert!(filter.enabled(&Level::DEBUG, "cortex_llm::client"));
        assert!(!filter.enabled(&Level::TRACE, "cortex_llm::client"));
        assert!(filter.enabled(&Level::TRACE, "cortex_llm::sse"));
        assert!(filter.enabled(&Level::TRACE, "cortex_llm::sse::parser"));
        assert!(!filter.enabled(&Level::ERROR, "cortex_tools::shell"));
        assert!(filter.enabled(&Level::INFO, "cortex_agent"));
        assert!(!filter.enabled(&Level::DEBUG, "cortex_agent"));
        assert_eq!(filter.most_detailed(), LevelFilter::TRACE);
        // The order they were written in does not matter.
        let other =
            Filter::parse("cortex_llm::sse=trace,cortex_tools=off,cortex_llm=debug").unwrap();
        assert_eq!(filter, other);
    }

    #[test]
    fn the_last_level_named_for_a_thing_stands() {
        let filter = Filter::parse("debug,cortex_llm=trace,warn,cortex_llm=error").unwrap();
        assert_eq!(filter.level_of("other"), LevelFilter::WARN);
        assert_eq!(filter.level_of("cortex_llm::x"), LevelFilter::ERROR);
    }

    #[test]
    fn what_is_not_a_level_is_refused_by_name() {
        for (text, part) in [
            ("loud", "loud"),
            ("info,cortex_llm=verbose", "cortex_llm=verbose"),
            ("=debug", "=debug"),
            ("cortex_llm=", "cortex_llm="),
        ] {
            let error = Filter::parse(text).unwrap_err();
            assert_eq!(error.part, part);
            assert!(error.to_string().contains(part));
        }
        // Names are read whatever their case.
        assert_eq!(
            Filter::parse("WARN").unwrap(),
            Filter::at(LevelFilter::WARN)
        );
    }
}
