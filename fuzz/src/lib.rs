//! What the fuzz targets check (`docs/testing-and-ci.md`, "Not blocking").
//!
//! Each target is a function from bytes to nothing, which panics when a
//! property does not hold. The fuzzer makes the bytes and follows coverage
//! to find the ones that reach new code.
//!
//! The input is read the same way by both targets: its first two bytes
//! decide where the rest is cut into the pieces a network would deliver,
//! and the rest is the stream. So the fuzzer explores the cuts as well as
//! the content.
//!
//! | Target | The properties |
//! |---|---|
//! | [`sse_parser`] | No input makes the parser panic. The same bytes give the same events, or the same error, however they are cut. |
//! | [`anthropic_decoder`] | No input makes the decoder panic. The same bytes give the same events however they are cut. A response ends at most once, and nothing follows its end. A block ends only after it started. Every block the decoder stores is one JSON object, so it can be sent back. |

use std::sync::Arc;

use cortex_llm::anthropic::{self, AnthropicMessages};
use cortex_llm::{Catalog, EventSink, ModelEntry, ModelEvent, ModelRequest, Protocol, SseParser};
use cortex_protocol::model::ModelRole;
use cortex_protocol::{ModelId, ProviderId, Speed};

/// Splits an input into the offsets at which the stream is cut, and the
/// stream. The first two bytes seed the sizes of the pieces, from one byte
/// to a few hundred.
fn cuts(data: &[u8]) -> (Vec<usize>, &[u8]) {
    let (seed, stream) = match data {
        [first, second, rest @ ..] => (u32::from(*first) << 8 | u32::from(*second), rest),
        _ => (0, data),
    };
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut cuts = Vec::new();
    let mut at = 0;
    // The top bits of the seed choose how large pieces may be.
    let largest = [1, 2, 7, 64, 400][(seed >> 13) as usize % 5];
    while at < stream.len() {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        at += 1 + (state >> 16) as usize % largest;
        cuts.push(at.min(stream.len()));
    }
    (cuts, stream)
}

/// The pieces of `stream` that `cuts` make.
fn pieces<'a>(stream: &'a [u8], cuts: &'a [usize]) -> impl Iterator<Item = &'a [u8]> {
    let mut from = 0;
    cuts.iter()
        .copied()
        .chain([stream.len()])
        .filter_map(move |end| {
            let end = end.min(stream.len());
            (end > from).then(|| {
                let piece = &stream[from..end];
                from = end;
                piece
            })
        })
}

fn parse(stream: &[u8], cuts: &[usize]) -> Result<Vec<(String, String)>, String> {
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    for piece in pieces(stream, cuts) {
        parser.push(piece);
        loop {
            match parser.next_event() {
                None => break,
                Some(Err(error)) => return Err(error.to_string()),
                Some(Ok(event)) => events.push((event.event.to_owned(), event.data.to_owned())),
            }
        }
    }
    Ok(events)
}

/// The SSE parser: the same events however the bytes are cut.
///
/// # Panics
///
/// When the property does not hold: that is how a fuzz target reports.
pub fn sse_parser(data: &[u8]) {
    let (cuts, stream) = cuts(data);
    let whole = parse(stream, &[]);
    let cut = parse(stream, &cuts);
    assert_eq!(whole, cut, "the cuts changed what was parsed");
}

fn model(id: &str) -> Arc<ModelEntry> {
    let catalog = Catalog::builtin().expect("the built-in catalog loads");
    let entry = catalog
        .get(&ProviderId::new(anthropic::PROVIDER), &ModelId::new(id))
        .expect("the built-in catalog has the model");
    Arc::clone(entry)
}

fn decode(request: &ModelRequest, stream: &[u8], cuts: &[usize]) -> Result<Vec<ModelEvent>, String> {
    let provider = anthropic::provider();
    let mut decoder = AnthropicMessages.decoder(request, &provider);
    let mut parser = SseParser::new();
    let mut sink = EventSink::sent_at(request.speed.clone());
    for piece in pieces(stream, cuts) {
        parser.push(piece);
        loop {
            match parser.next_event() {
                None => break,
                Some(Err(error)) => return Err(error.to_string()),
                Some(Ok(event)) => decoder
                    .feed(event, &mut sink)
                    .map_err(|error| error.message)?,
            }
        }
    }
    decoder.finish(&mut sink).map_err(|error| error.message)?;
    Ok(sink.take())
}

/// The decoder of the `anthropic-messages` protocol.
///
/// # Panics
///
/// When a property does not hold: that is how a fuzz target reports.
pub fn anthropic_decoder(data: &[u8]) {
    let (cuts, stream) = cuts(data);
    // Half the inputs are decoded as the response to a request at fast
    // speed, where an error in the stream can belong to the tier.
    let fast = data.first().is_some_and(|byte| byte & 1 == 1);
    let mut request = ModelRequest::new(model("claude-opus-5-5"), ModelRole::Main);
    if fast {
        request.speed = Speed::Fast;
    }

    let whole = decode(&request, stream, &[]);
    let cut = decode(&request, stream, &cuts);
    assert_eq!(whole, cut, "the cuts changed what was decoded");

    let Ok(events) = whole else { return };
    let endings = events.iter().filter(|event| event.is_terminal()).count();
    assert!(endings <= 1, "a response ended {endings} times");
    if endings == 1 {
        assert!(
            events.last().is_some_and(ModelEvent::is_terminal),
            "an event follows the end of the response"
        );
    }
    let mut started = Vec::new();
    for event in &events {
        match event {
            ModelEvent::BlockStart { index, .. } => {
                assert!(!started.contains(index), "the block {index} started twice");
                started.push(*index);
            }
            ModelEvent::BlockEnd { index, raw } => {
                assert!(started.contains(index), "the block {index} ended before it started");
                let stored: Result<serde_json::Value, _> = serde_json::from_str(raw.as_str());
                assert!(
                    stored.is_ok_and(|value| value.is_object()),
                    "a stored block is not one JSON object: {raw}"
                );
            }
            ModelEvent::TextDelta { index, .. }
            | ModelEvent::ReasoningDelta { index, .. }
            | ModelEvent::ToolCallStart { index, .. }
            | ModelEvent::ToolArgsDelta { index, .. } => {
                assert!(started.contains(index), "the block {index} has content before it started");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cuts_cover_the_stream_and_come_from_the_seed() {
        let data = [0x12, 0x34, b'a', b'b', b'c', b'd', b'e', b'f', b'g'];
        let (cuts, stream) = cuts(&data);
        assert_eq!(stream, b"abcdefg");
        assert_eq!(cuts.last(), Some(&stream.len()));
        assert!(cuts.windows(2).all(|pair| pair[0] < pair[1]));
        let joined: Vec<u8> = pieces(stream, &cuts).flatten().copied().collect();
        assert_eq!(joined, stream);
        // The same input, the same cuts.
        assert_eq!(super::cuts(&data).0, cuts);
        assert_eq!(super::cuts(&[]).1, b"");
        assert_eq!(super::cuts(&[7]).1, [7]);
    }

    #[test]
    fn both_targets_take_a_real_stream_and_nonsense() {
        let stream = b"\x00\x05event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"x\",\"usage\":{\"input_tokens\":1}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        sse_parser(stream);
        anthropic_decoder(stream);
        for nonsense in [&b""[..], b"\xff", b"\x01\x02data: {\n\n", b"zz\r\r\n\ndata\n\n: x\n"] {
            sse_parser(nonsense);
            anthropic_decoder(nonsense);
        }
    }
}
