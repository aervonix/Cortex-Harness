//! Speed tiers through the client (`docs/decisions/0027-speed-tiers.md`):
//! the request carries a speed, the usage carries the speed that ran, a
//! tier's own refusal is waited out when the wait is short and otherwise
//! falls back to standard and starts a cooldown, and none of it is silent.
//!
//! The cooldowns here start and end on the fake clock.

use cortex_base::ErrorKind;
use cortex_llm::{CooldownChange, ModelEvent, RetryPolicy, SpeedPolicy};
use cortex_protocol::Speed;
use cortex_protocol::speed::{CooldownState, SpeedFallbackBy, SpeedFallbackReason};
use cortex_testkit::protocol::sse;
use cortex_testkit::{Reply, fixtures};
use serde_json::json;

use crate::{
    HELLO, Harness, START, assert_kind, assert_well_formed, at, failure, fallbacks, final_error,
    final_usage, hello, hello_text, names, request_at, retries, secs, tier_refusal,
};

fn cooldowns(events: &[ModelEvent]) -> Vec<&CooldownChange> {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::SpeedCooldown(change) => Some(change),
            _ => None,
        })
        .collect()
}

fn priority() -> Speed {
    Speed::Other("priority".to_owned())
}

/// A good response whose usage says which speed served it.
fn hello_served_at(speed: &str) -> Reply {
    Reply::ok(hello_text(
        &json!({"input": 10, "output": 2, "speed": speed}),
    ))
}

fn no_fallback() -> RetryPolicy {
    RetryPolicy {
        speed: SpeedPolicy {
            fallback: false,
            ..SpeedPolicy::default()
        },
        ..RetryPolicy::default()
    }
}

// The request carries a speed, and the usage carries the speed that ran.

#[test]
fn the_request_says_its_speed_and_standard_is_said_too() {
    let harness = Harness::new([hello(), hello(), hello()]);
    harness.run_at(Speed::Standard);
    harness.run_at(Speed::Fast);
    harness.run_at(priority());
    assert_eq!(
        harness.speeds_sent(),
        ["standard", "fast", "priority"],
        "an explicit tier every time, never the provider's default"
    );
}

#[test]
fn a_response_that_reports_no_speed_ran_at_the_speed_that_was_sent() {
    let harness = Harness::new([hello()]);
    let events = harness.run_at(Speed::Fast);
    assert_eq!(names(&events), HELLO);
    let usage = final_usage(&events);
    assert_eq!(usage.speed, Speed::Fast);
    assert_eq!(usage.speed_requested, None);
}

#[test]
fn a_response_that_reports_another_speed_is_recorded_at_the_speed_it_reports() {
    // Asked for fast, served standard: in the usage, as providers that
    // downgrade by themselves report it.
    let harness = Harness::new([hello_served_at("standard"), hello_served_at("fast")]);
    let events = harness.run_at(Speed::Fast);
    assert_well_formed(&events);
    assert_eq!(
        names(&events),
        [
            "start",
            "block_start",
            "text",
            "block_end",
            "speed_fallback",
            "end"
        ],
        "the move is said as soon as the response reports it"
    );
    let usage = final_usage(&events);
    assert_eq!(
        usage.speed,
        Speed::Standard,
        "what ran, not what was asked for"
    );
    assert_eq!(
        usage.speed_requested,
        Some(Speed::Fast),
        "and what was asked for, beside it"
    );

    let change = fallbacks(&events)[0];
    assert_eq!(change.requested, Speed::Fast);
    assert_eq!(change.served, Speed::Standard);
    assert_eq!(
        change.by,
        SpeedFallbackBy::Provider,
        "marked as the provider's doing"
    );
    assert_eq!(change.reason, SpeedFallbackReason::Unspecified);
    assert_eq!(change.cooldown_until, None);

    // The provider's own downgrade starts no cooldown: the next request
    // asks for fast again, and this time gets it.
    assert!(cooldowns(&events).is_empty());
    let events = harness.run_at(Speed::Fast);
    assert_eq!(names(&events), HELLO);
    assert_eq!(harness.speeds_sent(), ["fast", "fast"]);
    let usage = final_usage(&events);
    assert_eq!(
        (usage.speed.clone(), usage.speed_requested.clone()),
        (Speed::Fast, None)
    );
    assert!(harness.timer.sleeps().is_empty());
}

#[test]
fn a_speed_reported_in_a_header_or_early_in_the_stream_counts_too() {
    // In a response header.
    let harness = Harness::new([hello().header("x-served-speed", "standard")]);
    let events = harness.run_at(priority());
    assert_eq!(names(&events)[0], "speed_fallback");
    assert_eq!(fallbacks(&events)[0].by, SpeedFallbackBy::Provider);
    assert_eq!(final_usage(&events).speed, Speed::Standard);
    assert_eq!(final_usage(&events).speed_requested, Some(priority()));

    // In a usage event in the middle, with an end that says nothing.
    let text = [
        sse("start", &json!({"id": "r", "model": "m"})),
        sse("usage", &json!({"input": 5, "speed": "standard"})),
        sse(
            "end",
            &json!({"stop": "end", "raw_stop": "done", "input": 5, "output": 1}),
        ),
    ]
    .concat();
    let harness = Harness::new([Reply::ok(text)]);
    let events = harness.run_at(Speed::Fast);
    assert_eq!(names(&events), ["start", "speed_fallback", "usage", "end"]);
    assert_eq!(fallbacks(&events).len(), 1, "said once");
    let usage = final_usage(&events);
    assert_eq!(
        (usage.speed.clone(), usage.speed_requested.clone()),
        (Speed::Standard, Some(Speed::Fast))
    );
}

#[test]
fn a_speed_the_model_does_not_offer_is_not_sent() {
    let harness = Harness::new([hello(), hello()]);
    // A model with no tiers at all.
    let events = harness.run_request(request_at(fixtures::plain_model(), Speed::Fast));
    assert_eq!(names(&events)[0], "speed_fallback");
    let change = fallbacks(&events)[0];
    assert_eq!(change.by, SpeedFallbackBy::Harness);
    assert_eq!(change.reason, SpeedFallbackReason::NotOffered);
    assert_eq!(
        (change.requested.clone(), change.served.clone()),
        (Speed::Fast, Speed::Standard)
    );
    assert_eq!(final_usage(&events).speed, Speed::Standard);
    assert_eq!(final_usage(&events).speed_requested, Some(Speed::Fast));

    // A tier this model does not have, on a model that has others.
    let events = harness.run_at(Speed::Other("ultrafast".to_owned()));
    assert_eq!(
        fallbacks(&events)[0].reason,
        SpeedFallbackReason::NotOffered
    );
    assert_eq!(harness.speeds_sent(), ["standard", "standard"]);
    assert!(cooldowns(&events).is_empty());
}

// A tier's own refusal.

#[test]
fn a_short_wait_is_waited_in_place_at_the_same_speed() {
    let refused = tier_refusal(429, "rate_limit").header("retry-after", "8");
    let harness = Harness::new([refused, hello()]);
    let events = harness.run_at(Speed::Fast);

    assert_eq!(
        names(&events),
        ["retry", "start", "block_start", "text", "block_end", "end"]
    );
    assert_eq!(
        harness.timer.sleeps(),
        [secs(8)],
        "exactly the provider's wait"
    );
    assert_eq!(harness.now(), at("2026-11-12T10:00:08Z"));
    assert_eq!(
        harness.speeds_sent(),
        ["fast", "fast"],
        "sent again at the same speed"
    );
    assert!(fallbacks(&events).is_empty() && cooldowns(&events).is_empty());
    assert_eq!(retries(&events)[0].error.tier, Some(Speed::Fast));
    let usage = final_usage(&events);
    assert_eq!(
        (usage.speed.clone(), usage.speed_requested.clone()),
        (Speed::Fast, None)
    );
}

#[test]
fn a_long_wait_falls_back_to_standard_at_once_and_starts_a_cooldown() {
    let refused = tier_refusal(429, "rate_limit").header("retry-after", "45");
    let harness = Harness::new([refused, hello(), hello(), hello(), hello()]);
    let events = harness.run_at(Speed::Fast);
    assert_well_formed(&events);

    assert_eq!(
        names(&events),
        [
            "speed_fallback",
            "speed_cooldown",
            "start",
            "block_start",
            "text",
            "block_end",
            "end"
        ]
    );
    assert!(harness.timer.sleeps().is_empty(), "nothing is waited for");
    assert_eq!(harness.now(), at(START), "the request goes on at once");
    assert_eq!(harness.speeds_sent(), ["fast", "standard"]);

    // The cooldown ends when the provider said the tier is back.
    let until = at("2026-11-12T10:00:45Z");
    let change = fallbacks(&events)[0];
    assert_eq!(change.by, SpeedFallbackBy::Harness);
    assert_eq!(change.reason, SpeedFallbackReason::RateLimited);
    assert_eq!(
        (change.requested.clone(), change.served.clone()),
        (Speed::Fast, Speed::Standard)
    );
    assert_eq!(change.cooldown_until, Some(until));
    assert_eq!(
        cooldowns(&events),
        [&CooldownChange {
            speed: Speed::Fast,
            state: CooldownState::Started,
            until: Some(until),
        }]
    );
    let usage = final_usage(&events);
    assert_eq!(
        (usage.speed.clone(), usage.speed_requested.clone()),
        (Speed::Standard, Some(Speed::Fast))
    );

    // During the cooldown a request that asks for fast is sent at standard,
    // and says why.
    harness.clock.advance(secs(44));
    let events = harness.run_at(Speed::Fast);
    assert_eq!(names(&events)[0], "speed_fallback");
    let change = fallbacks(&events)[0];
    assert_eq!(change.by, SpeedFallbackBy::Harness);
    assert_eq!(change.reason, SpeedFallbackReason::Cooldown);
    assert_eq!(change.cooldown_until, Some(until));
    assert!(cooldowns(&events).is_empty(), "no new cooldown");
    assert_eq!(final_usage(&events).speed_requested, Some(Speed::Fast));
    assert_eq!(harness.speeds_sent()[2], "standard");

    // A request that asks for standard has nothing to say about it.
    assert_eq!(names(&harness.run_at(Speed::Standard)), HELLO);

    // The cooldown ends on the fake clock. The next request says so, once,
    // and asks for fast again.
    harness.clock.advance(secs(1));
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        names(&events),
        [
            "speed_cooldown",
            "start",
            "block_start",
            "text",
            "block_end",
            "end"
        ]
    );
    assert_eq!(
        cooldowns(&events),
        [&CooldownChange {
            speed: Speed::Fast,
            state: CooldownState::Ended,
            until: None,
        }]
    );
    assert_eq!(harness.speeds_sent()[4], "fast");
    assert_eq!(final_usage(&events).speed, Speed::Fast);
    assert_eq!(harness.transport.remaining(), 0);
}

#[test]
fn an_unknown_wait_falls_back_and_cools_down_for_ten_minutes() {
    // A rate limit that names no wait, and an overload.
    for (refused, reason) in [
        (
            tier_refusal(429, "rate_limit"),
            SpeedFallbackReason::RateLimited,
        ),
        (
            tier_refusal(529, "overloaded"),
            SpeedFallbackReason::Overloaded,
        ),
    ] {
        let harness = Harness::new([refused, hello(), hello(), hello()]);
        let events = harness.run_at(Speed::Fast);
        let change = fallbacks(&events)[0];
        assert_eq!(change.reason, reason);
        assert_eq!(change.cooldown_until, Some(at("2026-11-12T10:10:00Z")));
        assert!(harness.timer.sleeps().is_empty());

        // One second before the ten minutes are up it still holds.
        harness.clock.advance(secs(599));
        assert_eq!(
            fallbacks(&harness.run_at(Speed::Fast))[0].reason,
            SpeedFallbackReason::Cooldown
        );
        harness.clock.advance(secs(1));
        let events = harness.run_at(Speed::Fast);
        assert_eq!(cooldowns(&events)[0].state, CooldownState::Ended);
        assert_eq!(
            harness.speeds_sent(),
            ["fast", "standard", "standard", "fast"]
        );
    }
}

#[test]
fn the_short_wait_is_a_budget_for_the_whole_request() {
    let refused = || tier_refusal(429, "rate_limit").header("retry-after", "8");
    let harness = Harness::new([refused(), refused(), refused(), hello()]);
    let events = harness.run_at(Speed::Fast);
    // 8 s and 8 s are within 20; a third 8 would not be.
    assert_eq!(harness.timer.sleeps(), [secs(8), secs(8)]);
    assert_eq!(harness.speeds_sent(), ["fast", "fast", "fast", "standard"]);
    assert_eq!(
        names(&events),
        [
            "retry",
            "retry",
            "speed_fallback",
            "speed_cooldown",
            "start",
            "block_start",
            "text",
            "block_end",
            "end"
        ]
    );
    // The cooldown runs from the fallback, for the wait the provider named.
    assert_eq!(
        fallbacks(&events)[0].cooldown_until,
        Some(at("2026-11-12T10:00:24Z"))
    );
}

#[test]
fn the_short_wait_and_the_cooldown_are_settings() {
    let policy = RetryPolicy {
        speed: SpeedPolicy {
            fallback: true,
            short_wait: secs(60),
            cooldown: secs(90),
        },
        ..RetryPolicy::default()
    };
    // 45 s is now short enough to wait.
    let harness = Harness::with_policy(
        [
            tier_refusal(429, "rate_limit").header("retry-after", "45"),
            hello(),
        ],
        policy.clone(),
    );
    harness.run_at(Speed::Fast);
    assert_eq!(harness.timer.sleeps(), [secs(45)]);
    assert_eq!(harness.speeds_sent(), ["fast", "fast"]);

    // And a refusal with no wait cools down for the configured 90 s.
    let harness = Harness::with_policy([tier_refusal(529, "overloaded"), hello()], policy);
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        fallbacks(&events)[0].cooldown_until,
        Some(at("2026-11-12T10:01:30Z"))
    );
}

#[test]
fn a_fallback_gives_standard_speed_its_own_attempts() {
    // Four ordinary failures at fast, then the tier's refusal on the fifth
    // and last attempt: the request falls back instead of failing.
    let harness = Harness::new([
        failure(503, "unavailable"),
        failure(503, "unavailable"),
        failure(503, "unavailable"),
        failure(503, "unavailable"),
        tier_refusal(429, "rate_limit").header("retry-after", "1"),
        hello(),
    ]);
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        names(&events)[..6],
        [
            "retry",
            "retry",
            "retry",
            "retry",
            "speed_fallback",
            "speed_cooldown"
        ]
    );
    assert_eq!(names(&events)[6..], HELLO);
    assert_eq!(harness.speeds_sent()[5], "standard");
}

#[test]
fn a_refusal_that_arrives_inside_the_stream_falls_back_too() {
    let refused = Reply::ok(sse("start", &json!({"id": "r1", "model": "m"})))
        .chunk(sse("error", &json!({"code": "overloaded", "tier": true})));
    let harness = Harness::new([refused, hello()]);
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        names(&events),
        [
            "start",
            "speed_fallback",
            "speed_cooldown",
            "start",
            "block_start",
            "text",
            "block_end",
            "end"
        ],
        "the first response is void: a new one starts after the fallback"
    );
    assert_eq!(
        fallbacks(&events)[0].reason,
        SpeedFallbackReason::Overloaded
    );
    assert_eq!(harness.speeds_sent(), ["fast", "standard"]);
}

#[test]
fn the_models_own_limit_is_not_the_tiers() {
    // A rate limit that does not name the tier, on a request sent at fast:
    // an ordinary rate limit. It waits 45 s and is sent at fast again.
    let limited = failure(429, "rate_limit").header("retry-after", "45");
    let harness = Harness::new([limited, hello()]);
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        names(&events),
        ["retry", "start", "block_start", "text", "block_end", "end"]
    );
    assert_eq!(harness.timer.sleeps(), [secs(45)]);
    assert_eq!(harness.speeds_sent(), ["fast", "fast"]);
    assert_eq!(retries(&events)[0].error.tier, None);
    assert!(fallbacks(&events).is_empty());
}

#[test]
fn with_fallback_off_a_tiers_refusal_is_an_ordinary_rate_limit() {
    // The same refusal that falls back by default: it waits the 45 s and is
    // sent at fast again.
    let refused = || tier_refusal(429, "rate_limit").header("retry-after", "45");
    let harness = Harness::with_policy([refused(), hello(), hello()], no_fallback());
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        names(&events),
        ["retry", "start", "block_start", "text", "block_end", "end"]
    );
    assert_eq!(harness.timer.sleeps(), [secs(45)]);
    assert_eq!(harness.speeds_sent(), ["fast", "fast"]);
    assert!(fallbacks(&events).is_empty() && cooldowns(&events).is_empty());
    assert_eq!(final_usage(&events).speed, Speed::Fast);
    // No cooldown was started: the next request is sent at fast.
    harness.run_at(Speed::Fast);
    assert_eq!(harness.speeds_sent()[2], "fast");

    // With no wait named, it backs off like any other.
    let harness = Harness::with_policy([tier_refusal(529, "overloaded"), hello()], no_fallback());
    harness.run_at(Speed::Fast);
    assert_eq!(harness.timer.sleeps().len(), 1);
    assert!(harness.timer.sleeps()[0] <= secs(1));
    assert_eq!(harness.speeds_sent(), ["fast", "fast"]);

    // A wait over the maximum ends the request with the error, at fast.
    let long = tier_refusal(429, "rate_limit").header("retry-after", "600");
    let harness = Harness::with_policy([long, hello()], no_fallback());
    let events = harness.run_at(Speed::Fast);
    assert_eq!(names(&events), ["error"]);
    let error = final_error(&events);
    assert_kind(error, ErrorKind::Transient, "rate_limit");
    assert_eq!(
        error.tier,
        Some(Speed::Fast),
        "the error still says whose limit it was"
    );
    assert_eq!(error.retry_after, Some(secs(600)));
    assert!(harness.timer.sleeps().is_empty());

    // And the attempts run out as for any failure.
    let harness = Harness::with_policy(
        (0..5).map(|_| tier_refusal(529, "overloaded")),
        no_fallback(),
    );
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        names(&events),
        ["retry", "retry", "retry", "retry", "error"]
    );
    assert_eq!(harness.speeds_sent(), ["fast"; 5]);
}

#[test]
fn a_cooldown_belongs_to_one_model_and_one_tier() {
    let harness = Harness::new([tier_refusal(529, "overloaded"), hello(), hello(), hello()]);
    harness.run_at(Speed::Fast);
    // Fast is cooling down on this model. Its other tier is not.
    let events = harness.run_at(priority());
    assert_eq!(names(&events), HELLO);
    assert_eq!(harness.speeds_sent()[2], "priority");

    // What the loop shows comes from the same table, and looking changes
    // nothing.
    let table = harness.client.cooldowns();
    let model = fixtures::model();
    let fast = table.in_effect(&model.info, &Speed::Fast);
    assert_eq!(fast.speed, Speed::Standard);
    assert_eq!(fast.reason, Some(SpeedFallbackReason::Cooldown));
    assert_eq!(fast.cooldown_until, Some(at("2026-11-12T10:10:00Z")));
    assert_eq!(table.in_effect(&model.info, &Speed::Fast), fast);

    let other = table.in_effect(&model.info, &priority());
    assert_eq!(
        (other.speed, other.reason, other.cooldown_until),
        (priority(), None, None)
    );
    let standard = table.in_effect(&model.info, &Speed::Standard);
    assert_eq!((standard.speed, standard.reason), (Speed::Standard, None));
    let not_offered = table.in_effect(&fixtures::plain_model().info, &Speed::Fast);
    assert_eq!(not_offered.speed, Speed::Standard);
    assert_eq!(not_offered.reason, Some(SpeedFallbackReason::NotOffered));
    assert_eq!(not_offered.cooldown_until, None);
}

#[test]
fn a_tier_refused_again_after_its_cooldown_cools_down_again() {
    let refused = || tier_refusal(429, "rate_limit").header("retry-after", "30");
    let harness = Harness::new([refused(), hello(), refused(), hello()]);
    harness.run_at(Speed::Fast);
    harness.clock.advance(secs(30));
    let events = harness.run_at(Speed::Fast);
    assert_eq!(
        names(&events)[..3],
        ["speed_cooldown", "speed_fallback", "speed_cooldown"],
        "the old cooldown ended, the tier was asked for, refused, and cools down again"
    );
    let changes = cooldowns(&events);
    assert_eq!(changes[0].state, CooldownState::Ended);
    assert_eq!(changes[1].state, CooldownState::Started);
    assert_eq!(changes[1].until, Some(at("2026-11-12T10:01:00Z")));
    assert_eq!(
        harness.speeds_sent(),
        ["fast", "standard", "fast", "standard"]
    );
}
