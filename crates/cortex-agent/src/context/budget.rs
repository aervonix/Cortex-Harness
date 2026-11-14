//! Budgeting (`docs/architecture/07-sessions-and-context.md`, "Budgeting").
//!
//! Before each request the loop estimates its size: what the provider
//! counted for the last request, plus what was added to the history since,
//! at a ratio of bytes to tokens measured from the session's own requests.
//! There is no tokenizer. The estimate is held against a limit, which is
//! the model's context window less room for the answer and a margin.

/// Bytes per token before the provider has counted a request. Low on
/// purpose: an estimate that is too high compacts a little early, one that
/// is too low sends a request that does not fit.
pub const DEFAULT_RATIO: f64 = 3.0;

/// The most room kept for the answer, in tokens, however much more the
/// model may write.
pub const OUTPUT_RESERVE_CAP: u64 = 32_768;

/// The most a request of a model may hold, in tokens: its context window,
/// less the room kept for the answer (the most the role may write, capped
/// at [`OUTPUT_RESERVE_CAP`] and at a quarter of the window), less a margin
/// of a fiftieth of the window.
#[must_use]
pub fn limit_of(window: u64, max_output: u64) -> u64 {
    let reserve = max_output.min(OUTPUT_RESERVE_CAP).min(window / 4);
    let margin = window / 50;
    window.saturating_sub(reserve).saturating_sub(margin)
}

/// What the last request that ended was counted as.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Counted {
    /// Its input, every class, and its output.
    tokens: u64,
    /// The size of the request's content once its answer was recorded.
    bytes_after: u64,
}

/// The session's estimate of a request's size.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Budget {
    counted: Option<Counted>,
    /// Bytes per token, from the last request the provider counted.
    ratio: Option<f64>,
}

impl Budget {
    /// Bytes per token.
    pub(crate) fn ratio(&self) -> f64 {
        self.ratio.unwrap_or(DEFAULT_RATIO)
    }

    /// Tokens for so many bytes.
    pub(crate) fn tokens(&self, bytes: u64) -> u64 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an estimate: a request is far below 2^52 bytes, and the ratio is positive"
        )]
        let tokens = (bytes as f64 / self.ratio()).ceil() as u64;
        tokens
    }

    /// The estimate for a request whose content is `bytes` now.
    pub(crate) fn estimate(&self, bytes: u64) -> u64 {
        match self.counted {
            Some(counted) => {
                counted.tokens + self.tokens(bytes.saturating_sub(counted.bytes_after))
            }
            None => self.tokens(bytes),
        }
    }

    /// A request whose content was `sent_bytes` was counted as `input`
    /// tokens (every class) and answered with `output`; with its answer the
    /// content is `bytes_after`.
    pub(crate) fn counted(&mut self, sent_bytes: u64, input: u64, output: u64, bytes_after: u64) {
        if input == 0 {
            // A provider that counted nothing says nothing about the ratio.
            return;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a ratio: the counts are far below 2^52"
        )]
        let ratio = (sent_bytes as f64 / input as f64).clamp(1.0, 8.0);
        self.ratio = Some(ratio);
        self.counted = Some(Counted {
            tokens: input + output,
            bytes_after,
        });
    }

    /// A new epoch began: the provider's count is of the old one. The ratio
    /// stays.
    pub(crate) fn new_epoch(&mut self) {
        self.counted = None;
    }
}

/// How full a request is, against the thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pressure {
    /// Below the threshold for a summary in the background.
    Fine,
    /// Past it: a summary is prepared while the session goes on.
    Prepare,
    /// Past the trigger: the epoch is compacted before the request.
    Compact,
}

/// Where `estimate` stands against `limit`.
pub(crate) fn pressure(estimate: u64, limit: u64, prepare_at: f64, compact_at: f64) -> Pressure {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a comparison of estimates far below 2^52"
    )]
    let (estimate, limit) = (estimate as f64, limit as f64);
    if estimate > compact_at * limit {
        Pressure::Compact
    } else if estimate > prepare_at * limit {
        Pressure::Prepare
    } else {
        Pressure::Fine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_keeps_room_for_the_answer_and_a_margin() {
        // A million tokens, an answer of up to 128,000: the room is capped.
        assert_eq!(limit_of(1_000_000, 128_000), 1_000_000 - 32_768 - 20_000);
        // A small local model: a quarter of the window at most for the answer.
        assert_eq!(limit_of(4_096, 2_048), 4_096 - 1_024 - 81);
        assert_eq!(limit_of(200_000, 8_000), 200_000 - 8_000 - 4_000);
        assert_eq!(limit_of(0, 100), 0);
    }

    #[test]
    fn the_estimate_is_what_was_counted_and_what_came_after() {
        let mut budget = Budget::default();
        assert_eq!(budget.estimate(3_000), 1_000, "the default ratio");
        budget.counted(8_000, 2_000, 100, 9_000);
        assert!((budget.ratio() - 4.0).abs() < f64::EPSILON);
        assert_eq!(budget.estimate(9_000), 2_100);
        assert_eq!(budget.estimate(13_000), 2_100 + 1_000);
        budget.new_epoch();
        assert_eq!(budget.estimate(4_000), 1_000, "the ratio stays");
    }

    #[test]
    fn the_thresholds_are_the_limits_fractions() {
        assert_eq!(pressure(69, 100, 0.7, 0.85), Pressure::Fine);
        assert_eq!(pressure(71, 100, 0.7, 0.85), Pressure::Prepare);
        assert_eq!(pressure(86, 100, 0.7, 0.85), Pressure::Compact);
    }
}
