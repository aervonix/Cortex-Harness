//! The events of a response stream, and the usage they report
//! (`docs/architecture/04-providers.md`, "Stream events" and "Usage and
//! cost").
//!
//! [`ModelEvent`] is what the agent loop reads. A decoder does not build
//! these directly: it writes what the provider said into an [`EventSink`],
//! which turns "the response reported this speed" into "the request ran at
//! this speed", in one place for every protocol.

use std::time::Duration;

use cortex_base::UtcTime;
use cortex_protocol::speed::{CooldownState, SpeedFallbackBy, SpeedFallbackReason};
use cortex_protocol::usage::{LimitWindow, TokenUsage};
use cortex_protocol::{RawJson, Speed, StopReason};

use crate::error::ProviderError;

/// What kind of content a block of a response holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// Text.
    Text,
    /// Reasoning: visible text or a summary, and an opaque payload.
    Reasoning,
    /// A call to a tool.
    ToolCall,
    /// Something the provider produced that the neutral model does not
    /// name: a server-side tool's use or result, a compaction block.
    ProviderBlock,
}

/// One event of a response stream.
///
/// A response is `Start`, then blocks, then `End`. Blocks are keyed by
/// `index`, and indexes need not be contiguous or arrive in order.
///
/// **A stream can hold more than one response.** When an attempt fails and
/// the request is sent again, the stream says so with [`ModelEvent::Retry`]
/// or, for a fallback to standard speed, [`ModelEvent::SpeedFallback`] by
/// the harness, and another `Start` follows. Everything between the earlier
/// `Start` and that point belongs to an attempt that no longer counts.
///
/// **The last event** is `End` or `Error`, exactly one of them.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelEvent {
    /// A response began.
    Start {
        /// The provider's id for the response.
        response_id: Option<String>,
        /// The model that is answering, as the provider names it.
        model: String,
    },
    /// A block began.
    BlockStart {
        /// The block's index in the response.
        index: u32,
        /// What it holds.
        kind: BlockKind,
    },
    /// More text of a text block.
    TextDelta {
        /// The block.
        index: u32,
        /// The text to append.
        text: String,
    },
    /// More of a reasoning block's visible text or summary.
    ReasoningDelta {
        /// The block.
        index: u32,
        /// The text to append.
        text: String,
    },
    /// A tool call was named. Its arguments follow as fragments.
    ToolCallStart {
        /// The block.
        index: u32,
        /// The provider's id for the call.
        id: String,
        /// The tool's name.
        name: String,
    },
    /// More of a tool call's arguments: a fragment of JSON text, not yet
    /// valid JSON by itself.
    ToolArgsDelta {
        /// The block.
        index: u32,
        /// The fragment to append.
        json: String,
    },
    /// A block is complete.
    BlockEnd {
        /// The block.
        index: u32,
        /// The provider's own JSON for the whole block, byte for byte.
        raw: RawJson,
    },
    /// What the request has used so far. It may arrive more than once, and
    /// the usage on `End` is the one that counts.
    Usage(Usage),
    /// The rate limits the provider reported with this response.
    Limits(Vec<LimitWindow>),
    /// The attempt failed and the request will be sent again after a wait.
    Retry(RetryInfo),
    /// The request runs, or ran, at another speed than was asked for.
    SpeedFallback(SpeedMove),
    /// A speed tier's cooldown began or ended.
    SpeedCooldown(CooldownChange),
    /// The response is complete.
    End {
        /// Why it ended, in the neutral set.
        stop: StopReason,
        /// The provider's own stop value.
        raw_stop: String,
        /// More about the stop where the provider gave it, such as the
        /// category of a refusal.
        stop_detail: Option<String>,
        /// What the request used. Authoritative.
        usage: Usage,
    },
    /// The request failed and will not be tried again here.
    Error(ProviderError),
}

impl ModelEvent {
    /// Whether this is the last event of a stream.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::End { .. } | Self::Error(_))
    }
}

/// What a request used, and the speed it ran at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    /// Tokens by class, normalised: the classes do not overlap.
    pub tokens: TokenUsage,
    /// The speed the request **ran at**: what the response reported, and
    /// where it reported nothing, the speed the request was sent at. This
    /// is the speed whose rate card prices the request.
    pub speed: Speed,
    /// The speed that was asked for, when the request ran at another one.
    /// `None` when it ran at the speed that was asked for.
    pub speed_requested: Option<Speed>,
    /// The cost the provider itself stated, in nano-dollars, where it
    /// states one. It overrides the calculation from the catalog's prices.
    pub provider_cost: Option<u64>,
    /// The provider's own usage object, byte for byte.
    pub raw: Option<RawJson>,
}

/// Usage as a decoder reads it from a response, before the speed the
/// request was sent at is taken into account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportedUsage {
    /// Tokens by class, normalised by the protocol.
    pub tokens: TokenUsage,
    /// The speed the response says served the request. `None` when this
    /// part of the response does not say: never a guess, and never the
    /// speed that was asked for.
    pub speed: Option<Speed>,
    /// The cost the provider stated, in nano-dollars.
    pub provider_cost: Option<u64>,
    /// The provider's own usage object.
    pub raw: Option<RawJson>,
}

/// An attempt failed and the request will be sent again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryInfo {
    /// Which attempt comes next, counting from 1.
    pub attempt: u32,
    /// The most attempts a request gets.
    pub max_attempts: u32,
    /// Why the last attempt failed.
    pub error: ProviderError,
    /// How long the wait is.
    pub wait: Duration,
    /// When the next attempt starts.
    pub next_attempt_at: UtcTime,
}

/// A request runs, or ran, at another speed than was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedMove {
    /// The speed the request asked for.
    pub requested: Speed,
    /// The speed it runs at, or ran at.
    pub served: Speed,
    /// Who moved it: the harness, before sending, or the provider, which
    /// served another speed and said so.
    pub by: SpeedFallbackBy,
    /// Why.
    pub reason: SpeedFallbackReason,
    /// When the harness will ask for the requested speed again, if the tier
    /// is in a cooldown. Never set for a move the provider made.
    pub cooldown_until: Option<UtcTime>,
}

/// A speed tier's cooldown began or ended, for the request's provider and
/// model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CooldownChange {
    /// The tier.
    pub speed: Speed,
    /// Whether the cooldown began or ended.
    pub state: CooldownState,
    /// When it ends, for one that began.
    pub until: Option<UtcTime>,
}

/// Where a decoder writes what it reads from a response.
///
/// The sink knows the speed the request asked for and the speed it was sent
/// at, and it is the one place that applies the rule of
/// `docs/decisions/0027-speed-tiers.md`: **a request is recorded at the
/// speed the response reports, and at the speed it was sent at only when the
/// response reports none.** A decoder passes on what the provider said
/// ([`EventSink::served_at`], [`ReportedUsage::speed`]) and decides nothing.
///
/// When the response reports another speed than was sent, the sink adds a
/// [`ModelEvent::SpeedFallback`] marked as the provider's doing, once.
#[derive(Debug)]
pub struct EventSink {
    requested: Speed,
    sent: Speed,
    reported: Option<Speed>,
    told: bool,
    request_id: Option<String>,
    events: Vec<ModelEvent>,
}

impl EventSink {
    /// A sink for a request that asked for `requested` and was sent at
    /// `sent`. The two differ when the harness had already moved the
    /// request, for a cooldown or because the model does not offer the
    /// speed.
    #[must_use]
    pub fn new(requested: Speed, sent: Speed) -> Self {
        Self {
            requested,
            sent,
            reported: None,
            told: false,
            request_id: None,
            events: Vec::new(),
        }
    }

    /// A sink for a request that was sent at the speed it asked for.
    #[must_use]
    pub fn sent_at(speed: Speed) -> Self {
        Self::new(speed.clone(), speed)
    }

    /// The speed the request was sent at.
    #[must_use]
    pub fn sent(&self) -> &Speed {
        &self.sent
    }

    /// The provider's id for the request, once a header or an event gave it.
    /// It is put on every error reported afterwards that has none.
    pub fn request_id(&mut self, id: impl Into<String>) {
        self.request_id = Some(id.into());
    }

    /// The id given by [`request_id`](Self::request_id), if any.
    #[must_use]
    pub fn known_request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// A response began.
    pub fn start(&mut self, response_id: Option<String>, model: impl Into<String>) {
        self.events.push(ModelEvent::Start {
            response_id,
            model: model.into(),
        });
    }

    /// A block began.
    pub fn block_start(&mut self, index: u32, kind: BlockKind) {
        self.events.push(ModelEvent::BlockStart { index, kind });
    }

    /// More text of a text block.
    pub fn text_delta(&mut self, index: u32, text: impl Into<String>) {
        self.events.push(ModelEvent::TextDelta {
            index,
            text: text.into(),
        });
    }

    /// More visible reasoning.
    pub fn reasoning_delta(&mut self, index: u32, text: impl Into<String>) {
        self.events.push(ModelEvent::ReasoningDelta {
            index,
            text: text.into(),
        });
    }

    /// A tool call was named.
    pub fn tool_call_start(&mut self, index: u32, id: impl Into<String>, name: impl Into<String>) {
        self.events.push(ModelEvent::ToolCallStart {
            index,
            id: id.into(),
            name: name.into(),
        });
    }

    /// A fragment of a tool call's arguments.
    pub fn tool_args_delta(&mut self, index: u32, json: impl Into<String>) {
        self.events.push(ModelEvent::ToolArgsDelta {
            index,
            json: json.into(),
        });
    }

    /// A block is complete, with the provider's own JSON for it.
    pub fn block_end(&mut self, index: u32, raw: RawJson) {
        self.events.push(ModelEvent::BlockEnd { index, raw });
    }

    /// The response says which speed served the request, outside its usage:
    /// in a header, or in a field of its own.
    pub fn served_at(&mut self, speed: Speed) {
        self.report(Some(speed));
    }

    /// The usage so far.
    pub fn usage(&mut self, usage: ReportedUsage) {
        let usage = self.resolve(usage);
        self.events.push(ModelEvent::Usage(usage));
    }

    /// The rate limits the provider reported.
    pub fn limits(&mut self, limits: Vec<LimitWindow>) {
        if !limits.is_empty() {
            self.events.push(ModelEvent::Limits(limits));
        }
    }

    /// The response is complete.
    pub fn end(
        &mut self,
        stop: StopReason,
        raw_stop: impl Into<String>,
        stop_detail: Option<String>,
        usage: ReportedUsage,
    ) {
        let usage = self.resolve(usage);
        self.events.push(ModelEvent::End {
            stop,
            raw_stop: raw_stop.into(),
            stop_detail,
            usage,
        });
    }

    /// The provider reported a failure in the stream.
    pub fn error(&mut self, mut error: ProviderError) {
        if error.request_id.is_none() {
            error.request_id.clone_from(&self.request_id);
        }
        self.events.push(ModelEvent::Error(error));
    }

    /// The events written so far.
    #[must_use]
    pub fn events(&self) -> &[ModelEvent] {
        &self.events
    }

    /// Takes the events written so far, leaving the sink ready for more.
    pub fn take(&mut self) -> Vec<ModelEvent> {
        std::mem::take(&mut self.events)
    }

    /// Notes a speed the response reported, and says so the first time it
    /// is not the speed the request was sent at.
    fn report(&mut self, speed: Option<Speed>) {
        let Some(speed) = speed else { return };
        if speed != self.sent && !self.told {
            self.told = true;
            self.events.push(ModelEvent::SpeedFallback(SpeedMove {
                requested: self.requested.clone(),
                served: speed.clone(),
                by: SpeedFallbackBy::Provider,
                reason: SpeedFallbackReason::Unspecified,
                cooldown_until: None,
            }));
        }
        self.reported = Some(speed);
    }

    fn resolve(&mut self, usage: ReportedUsage) -> Usage {
        self.report(usage.speed);
        // What the response reported, in this part or an earlier one;
        // without a report, what was sent.
        let ran = self.reported.clone().unwrap_or_else(|| self.sent.clone());
        let speed_requested = (ran != self.requested).then(|| self.requested.clone());
        Usage {
            tokens: usage.tokens,
            speed: ran,
            speed_requested,
            provider_cost: usage.provider_cost,
            raw: usage.raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::ErrorKind;

    use super::*;

    fn tokens(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input,
            output,
            ..TokenUsage::default()
        }
    }

    fn reported(speed: Option<Speed>) -> ReportedUsage {
        ReportedUsage {
            tokens: tokens(10, 5),
            speed,
            ..ReportedUsage::default()
        }
    }

    fn final_usage(sink: &EventSink) -> Usage {
        match sink.events().last() {
            Some(ModelEvent::End { usage, .. }) => usage.clone(),
            other => panic!("the last event is not an end: {other:?}"),
        }
    }

    fn fallbacks(sink: &EventSink) -> Vec<SpeedMove> {
        sink.events()
            .iter()
            .filter_map(|event| match event {
                ModelEvent::SpeedFallback(change) => Some(change.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_response_that_reports_no_speed_ran_at_the_speed_it_was_sent_at() {
        let mut sink = EventSink::sent_at(Speed::Fast);
        sink.end(StopReason::End, "end_turn", None, reported(None));
        let usage = final_usage(&sink);
        assert_eq!(usage.speed, Speed::Fast);
        assert_eq!(usage.speed_requested, None);
        assert_eq!(usage.tokens, tokens(10, 5));
        assert!(fallbacks(&sink).is_empty());
    }

    #[test]
    fn a_response_that_reports_the_speed_that_was_sent_says_nothing_more() {
        let mut sink = EventSink::sent_at(Speed::Fast);
        sink.end(
            StopReason::End,
            "end_turn",
            None,
            reported(Some(Speed::Fast)),
        );
        let usage = final_usage(&sink);
        assert_eq!((usage.speed, usage.speed_requested), (Speed::Fast, None));
        assert!(fallbacks(&sink).is_empty());
    }

    #[test]
    fn asked_for_fast_and_served_standard_is_recorded_at_standard_and_said_once() {
        let mut sink = EventSink::sent_at(Speed::Fast);
        sink.start(Some("resp_1".to_owned()), "a-model");
        sink.usage(reported(Some(Speed::Standard)));
        sink.usage(reported(Some(Speed::Standard)));
        sink.end(
            StopReason::End,
            "end_turn",
            None,
            reported(Some(Speed::Standard)),
        );

        let usage = final_usage(&sink);
        assert_eq!(usage.speed, Speed::Standard);
        assert_eq!(usage.speed_requested, Some(Speed::Fast));
        assert_eq!(
            fallbacks(&sink),
            [SpeedMove {
                requested: Speed::Fast,
                served: Speed::Standard,
                by: SpeedFallbackBy::Provider,
                reason: SpeedFallbackReason::Unspecified,
                cooldown_until: None,
            }],
            "the provider's move is said once, and starts no cooldown"
        );
        // The notice comes before the first usage that carries the speed.
        assert!(matches!(sink.events()[1], ModelEvent::SpeedFallback(_)));
        for event in sink.events() {
            if let ModelEvent::Usage(usage) = event {
                assert_eq!(usage.speed, Speed::Standard);
                assert_eq!(usage.speed_requested, Some(Speed::Fast));
            }
        }
    }

    #[test]
    fn a_speed_reported_early_holds_for_usage_that_reports_none() {
        let mut sink = EventSink::sent_at(Speed::Fast);
        sink.usage(reported(Some(Speed::Standard)));
        sink.end(StopReason::End, "end_turn", None, reported(None));
        let usage = final_usage(&sink);
        assert_eq!(usage.speed, Speed::Standard);
        assert_eq!(usage.speed_requested, Some(Speed::Fast));
    }

    #[test]
    fn a_speed_reported_in_a_header_counts_like_one_in_the_usage() {
        let mut sink = EventSink::sent_at(Speed::Other("priority".to_owned()));
        sink.served_at(Speed::Standard);
        sink.end(StopReason::End, "stop", None, reported(None));
        let usage = final_usage(&sink);
        assert_eq!(usage.speed, Speed::Standard);
        assert_eq!(
            usage.speed_requested,
            Some(Speed::Other("priority".to_owned()))
        );
        assert_eq!(fallbacks(&sink).len(), 1);
        assert_eq!(fallbacks(&sink)[0].by, SpeedFallbackBy::Provider);
    }

    #[test]
    fn a_request_the_harness_already_moved_keeps_what_was_asked_for() {
        // Asked for fast, sent at standard during a cooldown, served standard.
        let mut sink = EventSink::new(Speed::Fast, Speed::Standard);
        sink.end(
            StopReason::End,
            "end_turn",
            None,
            reported(Some(Speed::Standard)),
        );
        let usage = final_usage(&sink);
        assert_eq!(usage.speed, Speed::Standard);
        assert_eq!(usage.speed_requested, Some(Speed::Fast));
        assert!(
            fallbacks(&sink).is_empty(),
            "the provider served what was sent; the harness has said its own part"
        );
        assert_eq!(sink.sent(), &Speed::Standard);
    }

    #[test]
    fn a_response_faster_than_asked_is_recorded_as_it_ran_too() {
        let mut sink = EventSink::sent_at(Speed::Standard);
        sink.end(
            StopReason::End,
            "end_turn",
            None,
            reported(Some(Speed::Fast)),
        );
        let usage = final_usage(&sink);
        assert_eq!(usage.speed, Speed::Fast);
        assert_eq!(usage.speed_requested, Some(Speed::Standard));
        assert_eq!(fallbacks(&sink).len(), 1);
    }

    #[test]
    fn an_error_takes_the_request_id_the_sink_knows() {
        let mut sink = EventSink::sent_at(Speed::Standard);
        assert_eq!(sink.known_request_id(), None);
        sink.request_id("req_42");
        sink.error(ProviderError::new(
            ErrorKind::Transient,
            "overloaded_error",
            "Busy.",
        ));
        let own = ProviderError::new(ErrorKind::Provider, "bad", "No.")
            .with_request_id(Some("req_own".to_owned()));
        sink.error(own);
        let ids: Vec<_> = sink
            .take()
            .into_iter()
            .map(|event| match event {
                ModelEvent::Error(error) => error.request_id,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids, [Some("req_42".to_owned()), Some("req_own".to_owned())]);
        assert!(sink.events().is_empty(), "taking leaves the sink empty");
    }

    #[test]
    fn content_events_pass_through_in_order_and_empty_limits_are_dropped() {
        let mut sink = EventSink::sent_at(Speed::Standard);
        sink.start(None, "m");
        sink.block_start(0, BlockKind::Text);
        sink.text_delta(0, "Hel");
        sink.text_delta(0, "lo");
        sink.block_end(0, RawJson::new(r#"{"type":"text","text":"Hello"}"#));
        sink.block_start(1, BlockKind::Reasoning);
        sink.reasoning_delta(1, "hm");
        sink.block_start(2, BlockKind::ToolCall);
        sink.tool_call_start(2, "call_1", "read");
        sink.tool_args_delta(2, r#"{"path":"#);
        sink.limits(Vec::new());
        let kinds: Vec<&str> = sink
            .events()
            .iter()
            .map(|event| match event {
                ModelEvent::Start { .. } => "start",
                ModelEvent::BlockStart { .. } => "block_start",
                ModelEvent::TextDelta { .. } => "text",
                ModelEvent::ReasoningDelta { .. } => "reasoning",
                ModelEvent::ToolCallStart { .. } => "tool",
                ModelEvent::ToolArgsDelta { .. } => "args",
                ModelEvent::BlockEnd { .. } => "block_end",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "start",
                "block_start",
                "text",
                "text",
                "block_end",
                "block_start",
                "reasoning",
                "block_start",
                "tool",
                "args"
            ]
        );
        assert!(!sink.events()[0].is_terminal());
        assert!(ModelEvent::Error(ProviderError::interrupted()).is_terminal());
    }
}
