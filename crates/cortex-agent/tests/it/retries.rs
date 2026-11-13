//! What the loop makes of an attempt that failed
//! (`docs/architecture/03-agent-loop.md`, "Retries"). The retry layer is
//! inside the model client; the loop sees each failed attempt as a `Retry`
//! event and the next response's start.

use cortex_agent::StatEvent;
use cortex_base::ErrorKind;
use cortex_llm::ModelEvent;
use cortex_llm::error::ProviderError;
use cortex_protocol::event::{Notice, TurnOutcome};
use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{Item, Part, StopReason};
use cortex_testkit::Response;

use crate::transitions::{retry, start};
use crate::{Rig, assert_valid_history, last_request, says};

#[test]
fn a_failed_attempt_is_shown_cleared_kept_in_the_log_and_left_out_of_the_context() {
    let response = Response::new()
        .text("Half an ans")
        .event(retry(2))
        .event(start())
        .text("The whole answer.")
        .end(StopReason::End);
    let (world, rig) = Rig::one("retry_visible", [response, says("Next.")]);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });

    assert_eq!(
        rig.events.labels(),
        [
            "turn.started",
            "user",
            "started:assistant",
            "delta:text",
            "notice:retry",
            "reset",
            "delta:text",
            "assistant",
            "turn.completed:completed",
            "idle"
        ],
        "one item, started once, cleared once"
    );
    let notices = rig.events.notices();
    let [Notice::Retry(notice)] = notices.as_slice() else {
        panic!("{notices:#?}");
    };
    assert_eq!((notice.attempt, notice.max_attempts), (2, 5));
    assert_eq!(notice.error.code, "overloaded");
    assert_eq!(notice.next_attempt_at.as_str(), "2026-11-13T10:00:02Z");
    assert!(
        notice.message.contains("attempt 2 of 5"),
        "{}",
        notice.message
    );

    // The partial response is an `attempt` entry, under the id of the item
    // it would have become.
    let attempts = rig.store.attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        (attempts[0].item_id.as_str(), attempts[0].attempt),
        ("i_2", 1)
    );
    assert_eq!(attempts[0].error.code, "overloaded");
    let [Part::Text(partial)] = attempts[0].partial.as_slice() else {
        panic!("{:#?}", attempts[0].partial);
    };
    assert_eq!(partial.text, "Half an ans");

    // The item holds only the attempt that counted.
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("an assistant item");
    };
    assert_eq!(assistant.id.as_str(), "i_2");
    let [Part::Text(text)] = assistant.parts.as_slice() else {
        panic!("{:#?}", assistant.parts);
    };
    assert_eq!(text.text, "The whole answer.");

    let retries = rig.stats.all().into_iter().find_map(|event| match event {
        StatEvent::Request(request) => Some(request.retries),
        _ => None,
    });
    assert_eq!(retries, Some(1));

    // And the next request never sees the discarded text.
    world.run(async {
        rig.prompt("more");
        rig.events.turns(2).await;
    });
    let request = last_request(&world.provider);
    assert_valid_history(&request.messages);
    assert!(!format!("{:?}", request.messages).contains("Half an ans"));
}

#[test]
fn attempts_that_run_out_end_the_turn_with_the_last_error() {
    let gave_up = ProviderError::new(ErrorKind::Transient, "overloaded", "Still busy.");
    let response = Response::events([
        retry(2),
        retry(3),
        start(),
        ModelEvent::TextDelta {
            index: 0,
            text: "Almost".to_owned(),
        },
        retry(4),
        ModelEvent::Error(gave_up),
    ]);
    let (world, rig) = Rig::one("retries_exhausted", [response]);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });

    let ends = rig.events.ends();
    assert_eq!(ends[0].outcome, TurnOutcome::Failed);
    let error = ends[0].error.as_ref().unwrap();
    assert_eq!(
        (error.kind.clone(), error.code.as_str()),
        (cortex_protocol::ErrorKind::Transient, "overloaded")
    );
    assert_eq!(rig.events.notices().len(), 3, "each wait was shown");
    assert_eq!(rig.store.attempts().len(), 3);
    // The item that had started ends with the error, and with nothing of
    // the attempts that were thrown away.
    let Item::AssistantMessage(assistant) = &rig.store.items()[1] else {
        panic!("an assistant item");
    };
    assert_eq!(assistant.stop_reason, Some(StopReason::Error));
    assert!(assistant.parts.is_empty());
    assert_eq!(
        world.provider.requests().len(),
        1,
        "the client retried, not the loop"
    );
}

#[test]
fn what_a_discarded_attempt_used_is_still_counted() {
    let first = TokenUsage {
        input: 500,
        output: 20,
        ..TokenUsage::default()
    };
    // The scripted response reports the same tokens at its end.
    let response = Response::new()
        .tokens(first)
        .text("Half")
        .usage()
        .event(retry(2))
        .event(start())
        .text("Whole.")
        .end(StopReason::End);
    let (world, rig) = Rig::one("retry_usage", [response]);
    world.run(async {
        rig.prompt("go");
        rig.events.turns(1).await;
    });
    let usage = rig.handle.usage();
    assert_eq!(
        usage.totals.requests, 2,
        "the attempt that failed was billed too"
    );
    assert_eq!(usage.totals.tokens.input, 1_000);
    assert_eq!(rig.events.ends()[0].usage.tokens.output, 40);
}
