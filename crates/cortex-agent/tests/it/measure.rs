//! Measurements, run by hand in a release build
//! (`docs/performance-budgets.md`, "Latency the harness adds"):
//!
//! ```text
//! cargo nextest run -p cortex-agent --cargo-profile release --run-ignored only \
//!     -E 'test(/^measure::/)' --no-capture
//! ```

use std::time::{Duration, Instant};

use cortex_protocol::item::{
    AssistantMessageItem, ToolOutcome, ToolResultItem, UserMessageItem,
};
use cortex_protocol::message::{Origin, ReasoningPart, TextPart, ToolCallPart};
use cortex_protocol::{CallId, Item, ItemId, ModelId, Part, ProviderId, RawJson, StopReason, TurnId};
use cortex_agent::MemorySessionStore;
use serde_json::json;

use crate::{Setup, World, says};

/// A history of about 200,000 tokens: 1,000 rounds of a prompt, a
/// response with reasoning, text and a call, each with its provider's own
/// blocks, and a result.
fn long_history() -> Vec<Item> {
    let mut items = Vec::new();
    let mut id = 0u32;
    let mut next = || {
        id += 1;
        ItemId::new(format!("i_{id}"))
    };
    let origin = Origin {
        protocol: cortex_testkit::fixtures::PROTOCOL.to_owned(),
        provider: ProviderId::new(cortex_testkit::fixtures::PROVIDER),
        model: ModelId::new(cortex_testkit::fixtures::MODEL),
        account_scope: None,
    };
    for round in 0..1_000 {
        items.push(Item::UserMessage(UserMessageItem {
            id: next(),
            parts: vec![Part::text(format!("Look at module {round} and say what it does."))],
            delivery: None,
            created_at: Some("2026-11-14T09:00:00Z".into()),
        }));
        let call = CallId::new(format!("c{round}"));
        let text = "x".repeat(200);
        items.push(Item::AssistantMessage(AssistantMessageItem {
            id: next(),
            parts: vec![
                Part::Reasoning(ReasoningPart {
                    text: "Thinking about it.".to_owned(),
                    raw: Some(RawJson::new(format!(
                        r#"{{"type":"thinking","thinking":"","signature":"{}"}}"#,
                        "s".repeat(120)
                    ))),
                }),
                Part::Text(TextPart {
                    text: text.clone(),
                    raw: Some(RawJson::new(format!(r#"{{"type":"text","text":"{text}"}}"#))),
                }),
                Part::ToolCall(ToolCallPart {
                    id: call.clone(),
                    name: "read".to_owned(),
                    arguments: json!({"path": format!("src/module_{round}.rs")}),
                    raw: None,
                }),
            ],
            stop_reason: Some(StopReason::ToolCalls),
            raw_stop: None,
            stop_detail: None,
            truncated: false,
            origin: Some(origin.clone()),
            usage: None,
            error: None,
            created_at: None,
        }));
        items.push(Item::ToolResult(ToolResultItem {
            id: next(),
            call_id: call,
            tool: "read".to_owned(),
            outcome: ToolOutcome::Success,
            content: vec![Part::text("fn line() {}\n".repeat(30))],
            payload: None,
            artifacts: Vec::new(),
            duration_ms: None,
            created_at: None,
        }));
    }
    items
}

#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn measure_prompt_to_request_with_a_long_history() {
    let world = World::new("measure_prompt", []);
    let mut setup = Setup::new(&world);
    setup.memory = MemorySessionStore::with_items(&TurnId::new("t_1"), long_history());
    let rig = setup.start(&world);
    // Room for the whole history: nothing is compacted.
    rig.window(10_000_000);
    let mut times: Vec<Duration> = Vec::new();
    for turn in 0..16 {
        world.provider.push(says("Fine."));
        let start = Instant::now();
        world.run(async {
            rig.prompt(&format!("Prompt {turn}."));
            // The response is taken when the request reaches the provider.
            // (`requests()` copies every request so far: not here.)
            while world.provider.remaining() > 0 {
                tokio::task::yield_now().await;
            }
        });
        let taken = start.elapsed();
        world.run(async {
            rig.events.turns(turn + 1).await;
        });
        // The first turn also builds the epoch's prompt.
        if turn > 0 {
            times.push(taken);
        }
    }
    let request = world.provider.requests().pop().unwrap();
    let bytes: usize = request
        .messages
        .iter()
        .map(|message| serde_json::to_string(message).unwrap().len())
        .sum();
    times.sort();
    println!(
        "prompt → request with {} messages ({} KB of messages): median {:?}, slowest {:?}",
        request.messages.len(),
        bytes / 1024,
        times[times.len() / 2],
        times[times.len() - 1]
    );
}
