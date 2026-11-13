//! Measurements, run by hand in a release build:
//!
//! ```text
//! cargo test --release -p cortex-llm --test it -- --ignored anthropic::measure --nocapture
//! ```
//!
//! What they measure is what the adapter adds to every request: building
//! the body from a long history, and decoding a response. The numbers are
//! in `docs/performance-budgets.md`.

#![allow(clippy::print_stdout, reason = "a measurement prints what it measured")]

use std::time::Instant;

use cortex_llm::anthropic::AnthropicMessages;
use cortex_llm::{ModelRequest, Protocol};

use super::scenario;
use super::{HAIKU, decode_whole, load, provider, user};

/// A conversation of `rounds` tool rounds: the recorded first round of the
/// conversation, with its reasoning and its two calls, over and over.
fn long_history(rounds: usize) -> ModelRequest {
    let first = scenario::desk(HAIKU, vec![user(scenario::PROMPTS[0])]);
    let events = decode_whole(&first, &load("conversation-1").response);
    let one = scenario::after(first.messages.clone(), &events, &first).expect("a round");
    let mut history = Vec::new();
    for _ in 0..rounds {
        history.extend(one.iter().cloned());
    }
    scenario::desk(HAIKU, history)
}

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

#[test]
#[ignore = "a measurement; run it in a release build"]
fn building_a_request_from_a_long_history() {
    for rounds in [10, 100, 1000] {
        let request = long_history(rounds);
        let size = AnthropicMessages
            .build(&request, &provider())
            .unwrap()
            .body
            .len();
        let times: Vec<f64> = (0..15)
            .map(|_| {
                let started = Instant::now();
                let http = AnthropicMessages.build(&request, &provider()).unwrap();
                let took = started.elapsed().as_secs_f64() * 1e6;
                assert_eq!(http.body.len(), size);
                took
            })
            .collect();
        println!(
            "build: {rounds} tool rounds, {} messages, a body of {size} bytes: {:.0} µs (median of 15)",
            request.messages.len(),
            median(times)
        );
    }
}

#[test]
#[ignore = "a measurement; run it in a release build"]
fn decoding_a_recorded_response() {
    for name in ["text", "tool-calls", "switch-2"] {
        let stream = load(name).response;
        let request = scenario::text();
        let events = decode_whole(&request, &stream).len();
        let times: Vec<f64> = (0..200)
            .map(|_| {
                let started = Instant::now();
                let decoded = decode_whole(&request, &stream);
                let took = started.elapsed().as_secs_f64() * 1e6;
                assert_eq!(decoded.len(), events);
                took
            })
            .collect();
        let micros = median(times);
        println!(
            "decode: {name}, {} bytes, {events} events: {micros:.1} µs, {:.0} MB/s (median of 200)",
            stream.len(),
            stream.len() as f64 / micros
        );
    }
}
