//! A parser for server-sent events: the framing every provider streams in
//! (`docs/architecture/04-providers.md`, "Stream events").
//!
//! It takes the bytes of a response body in whatever pieces the network
//! delivered them and gives back whole events. It follows the event-stream
//! format of the HTML standard, and does nothing more:
//!
//! - an event is a run of lines ended by a blank line;
//! - a line ends with `\n`, `\r\n` or `\r`, and the two bytes of a `\r\n`
//!   may arrive in different chunks;
//! - `event:` names the event and `data:` adds a line to its data; several
//!   `data:` lines are joined with `\n`; one space after the colon is not
//!   part of the value;
//! - a line that starts with `:` is a comment (providers send them as
//!   keep-alive pings), and a field it does not know is skipped;
//! - an event with no `data:` line is not an event;
//! - a byte-order mark at the very start is skipped;
//! - whatever follows the last blank line when the stream ends is dropped.
//!
//! There is no reconnection: `id:` and `retry:` are read and ignored. A
//! response that breaks off is sent again by the retry layer, not resumed.
//!
//! The parser does no I/O and keeps one event's worth of memory, so it is
//! tested by feeding it the same bytes cut at every possible place.

/// One event of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SseEvent<'a> {
    /// The event's name. `message` when the stream gave none, as the format
    /// defines.
    pub event: &'a str,
    /// The data: the `data:` lines, joined with `\n`.
    pub data: &'a str,
}

/// The name of an event whose stream gave it none.
pub const DEFAULT_EVENT: &str = "message";

/// The largest event the parser accepts unless told otherwise: 32 MiB. A
/// streamed event is a few hundred bytes; the limit is there so that a
/// server that never sends a blank line cannot fill the memory.
pub const DEFAULT_MAX_EVENT_LEN: usize = 32 * 1024 * 1024;

/// Why a stream could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    /// One event is larger than the parser's limit.
    #[error("an event in the stream is larger than the limit of {limit} bytes")]
    EventTooLong {
        /// The limit, in bytes.
        limit: usize,
    },
}

/// An incremental parser of an event stream.
///
/// Give it bytes with [`push`](Self::push), then take events with
/// [`next_event`](Self::next_event) until it has none.
#[derive(Debug)]
pub struct SseParser {
    /// Bytes received and not yet read as whole lines, from `start` on.
    buffer: Vec<u8>,
    /// Where the line being read begins.
    start: usize,
    /// How far the search for the line's end has got.
    scan: usize,
    /// The last line ended with a `\r` that was the last byte received. If
    /// the next byte is `\n` it belongs to that line ending.
    skip_lf: bool,
    /// Nothing has been read yet, so a byte-order mark may come first.
    at_start: bool,
    /// The event being built.
    event: String,
    data: String,
    has_data: bool,
    /// The event last returned. Borrowed by the caller until the next call.
    ready_event: String,
    ready_data: String,
    max_event_len: usize,
    failed: bool,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    /// A parser at the start of a stream.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_event_len(DEFAULT_MAX_EVENT_LEN)
    }

    /// A parser that refuses an event larger than `max_event_len` bytes.
    #[must_use]
    pub fn with_max_event_len(max_event_len: usize) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            scan: 0,
            skip_lf: false,
            at_start: true,
            event: String::new(),
            data: String::new(),
            has_data: false,
            ready_event: String::new(),
            ready_data: String::new(),
            max_event_len,
            failed: false,
        }
    }

    /// Adds bytes of the stream, cut wherever the network cut them.
    pub fn push(&mut self, mut bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.skip_lf {
            self.skip_lf = false;
            if bytes[0] == b'\n' {
                bytes = &bytes[1..];
            }
        }
        // Make room by dropping what has been read, before growing.
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
            self.scan = 0;
        } else if self.start >= 4096 && self.start >= self.buffer.len() / 2 {
            self.buffer.drain(..self.start);
            self.scan -= self.start;
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole event, or `None` when more bytes are needed.
    ///
    /// After an error the parser gives the same error again: the stream is
    /// not one it can go on reading.
    pub fn next_event(&mut self) -> Option<Result<SseEvent<'_>, SseError>> {
        if self.failed {
            return Some(Err(self.too_long()));
        }
        loop {
            let found = self.buffer[self.scan..]
                .iter()
                .position(|byte| matches!(byte, b'\n' | b'\r'));
            let Some(offset) = found else {
                self.scan = self.buffer.len();
                if self.event_len() > self.max_event_len {
                    self.failed = true;
                    return Some(Err(self.too_long()));
                }
                return None;
            };
            let end = self.scan + offset;
            let mut next = end + 1;
            if self.buffer[end] == b'\r' {
                match self.buffer.get(next) {
                    Some(b'\n') => next += 1,
                    Some(_) => {}
                    None => self.skip_lf = true,
                }
            }
            let mut line = &self.buffer[self.start..end];
            if self.at_start {
                self.at_start = false;
                line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
            }
            let dispatch = line.is_empty();
            if !dispatch {
                read_field(line, &mut self.event, &mut self.data, &mut self.has_data);
            }
            self.start = next;
            self.scan = next;

            if dispatch {
                if self.has_data {
                    // Each data line added a `\n`; the last one is not part
                    // of the data.
                    self.data.pop();
                    std::mem::swap(&mut self.event, &mut self.ready_event);
                    std::mem::swap(&mut self.data, &mut self.ready_data);
                    self.event.clear();
                    self.data.clear();
                    self.has_data = false;
                    let event = if self.ready_event.is_empty() {
                        DEFAULT_EVENT
                    } else {
                        self.ready_event.as_str()
                    };
                    return Some(Ok(SseEvent {
                        event,
                        data: &self.ready_data,
                    }));
                }
                // A blank line with no data before it: a name alone is not
                // an event.
                self.event.clear();
            } else if self.event_len() > self.max_event_len {
                self.failed = true;
                return Some(Err(self.too_long()));
            }
        }
    }

    /// Whether the stream stopped in the middle of an event: bytes or
    /// fields have arrived that no blank line has closed. At the end of a
    /// stream they are dropped, as the format says.
    #[must_use]
    pub fn is_mid_event(&self) -> bool {
        self.start < self.buffer.len() || self.has_data || !self.event.is_empty()
    }

    /// The bytes the event being built holds so far.
    fn event_len(&self) -> usize {
        self.data.len() + self.event.len() + (self.buffer.len() - self.start)
    }

    fn too_long(&self) -> SseError {
        SseError::EventTooLong {
            limit: self.max_event_len,
        }
    }
}

/// Reads one line that is not blank into the event being built.
fn read_field(line: &[u8], event: &mut String, data: &mut String, has_data: &mut bool) {
    let (name, value) = match line.iter().position(|byte| *byte == b':') {
        // A comment.
        Some(0) => return,
        Some(colon) => {
            let value = &line[colon + 1..];
            (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
        }
        // A line with no colon is a field with an empty value.
        None => (line, &[][..]),
    };
    match name {
        b"data" => {
            data.push_str(&String::from_utf8_lossy(value));
            data.push('\n');
            *has_data = true;
        }
        b"event" => {
            event.clear();
            event.push_str(&String::from_utf8_lossy(value));
        }
        // `id`, `retry` and anything a later version of the format adds.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use cortex_testkit::cases::{self, ensure};

    use super::*;

    /// A stream and the events it holds, as `(name, data)`.
    struct Fixture {
        name: &'static str,
        stream: &'static [u8],
        events: &'static [(&'static str, &'static str)],
    }

    const FIXTURES: &[Fixture] = &[
        Fixture {
            name: "named events with JSON data",
            stream: b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: \
                      content_block_delta\ndata: {\"delta\":{\"text\":\"Hel\"}}\n\nevent: \
                      message_stop\ndata: {}\n\n",
            events: &[
                ("message_start", "{\"type\":\"message_start\"}"),
                ("content_block_delta", "{\"delta\":{\"text\":\"Hel\"}}"),
                ("message_stop", "{}"),
            ],
        },
        Fixture {
            name: "unnamed events and a done marker",
            stream: b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\ndata: [DONE]\n\n",
            events: &[
                ("message", "{\"choices\":[{\"delta\":{\"content\":\"a\"}}]}"),
                ("message", "[DONE]"),
            ],
        },
        Fixture {
            name: "carriage return and line feed",
            stream: b"event: a\r\ndata: one\r\n\r\nevent: b\r\ndata: two\r\n\r\n",
            events: &[("a", "one"), ("b", "two")],
        },
        Fixture {
            name: "carriage return alone",
            stream: b"event: a\rdata: one\r\revent: b\rdata: two\r\r",
            events: &[("a", "one"), ("b", "two")],
        },
        Fixture {
            name: "mixed line endings",
            stream: b"data: one\r\n\ndata: two\r\rdata: three\n\r\n",
            events: &[("message", "one"), ("message", "two"), ("message", "three")],
        },
        Fixture {
            name: "several data lines are joined",
            stream: b"data: first\ndata: second\ndata:\ndata: fourth\n\n",
            events: &[("message", "first\nsecond\n\nfourth")],
        },
        Fixture {
            name: "comments and pings",
            stream: b": ping\n\n:\n: another\ndata: x\n: inside\n\n: trailing\n\n",
            events: &[("message", "x")],
        },
        Fixture {
            name: "space after the colon",
            stream: b"data:no space\n\ndata:  two spaces\n\ndata: \n\ndata\n\n",
            events: &[
                ("message", "no space"),
                ("message", " two spaces"),
                ("message", ""),
                ("message", ""),
            ],
        },
        Fixture {
            name: "fields the parser does not use",
            stream: b"id: 7\nretry: 3000\nfuture: x\nevent: e\ndata: d\n\nid\n\n",
            events: &[("e", "d")],
        },
        Fixture {
            name: "a name with no data is not an event",
            stream: b"event: lonely\n\ndata: kept\n\n",
            events: &[("message", "kept")],
        },
        Fixture {
            name: "the last name wins and colons stay in the value",
            stream: b"event: first\nevent: second\ndata: a: b: c\n\n",
            events: &[("second", "a: b: c")],
        },
        Fixture {
            name: "a byte-order mark at the start",
            stream: b"\xEF\xBB\xBFdata: after the mark\n\n",
            events: &[("message", "after the mark")],
        },
        Fixture {
            name: "characters of two, three and four bytes",
            stream: "event: d\u{e9}j\u{e0}\ndata: {\"text\":\"\u{4f60}\u{597d} \u{1f600} na\u{ef}ve\"}\n\n"
                .as_bytes(),
            events: &[("d\u{e9}j\u{e0}", "{\"text\":\"\u{4f60}\u{597d} \u{1f600} na\u{ef}ve\"}")],
        },
        Fixture {
            name: "an unfinished event at the end is dropped",
            stream: b"data: whole\n\nevent: cut\ndata: {\"partial\":",
            events: &[("message", "whole")],
        },
        Fixture {
            name: "an event without its blank line is dropped",
            stream: b"data: whole\n\ndata: no blank line after\n",
            events: &[("message", "whole")],
        },
        Fixture {
            name: "blank lines alone",
            stream: b"\n\n\r\n\r\ndata: x\n\n\n\n",
            events: &[("message", "x")],
        },
        Fixture {
            name: "nothing at all",
            stream: b"",
            events: &[],
        },
    ];

    type Events = Vec<(String, String)>;

    /// Parses a stream delivered in the given chunks.
    fn parse(chunks: &[&[u8]]) -> Events {
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            parser.push(chunk);
            while let Some(event) = parser.next_event() {
                let event = event.expect("no fixture is over the limit");
                events.push((event.event.to_owned(), event.data.to_owned()));
            }
        }
        events
    }

    fn expected(fixture: &Fixture) -> Events {
        fixture
            .events
            .iter()
            .map(|(event, data)| ((*event).to_owned(), (*data).to_owned()))
            .collect()
    }

    #[test]
    fn every_fixture_parses_whole() {
        for fixture in FIXTURES {
            assert_eq!(
                parse(&[fixture.stream]),
                expected(fixture),
                "{}",
                fixture.name
            );
        }
    }

    #[test]
    fn every_fixture_parses_split_at_every_byte() {
        for fixture in FIXTURES {
            let stream = fixture.stream;
            for cut in 0..=stream.len() {
                assert_eq!(
                    parse(&[&stream[..cut], &stream[cut..]]),
                    expected(fixture),
                    "{}: cut at byte {cut}",
                    fixture.name
                );
            }
        }
    }

    #[test]
    fn every_fixture_parses_one_byte_at_a_time() {
        for fixture in FIXTURES {
            let chunks: Vec<&[u8]> = fixture.stream.chunks(1).collect();
            assert_eq!(parse(&chunks), expected(fixture), "{}", fixture.name);
        }
    }

    #[test]
    fn every_fixture_parses_split_in_three() {
        // Every pair of cuts. The work grows with the square of the length,
        // so a fixture longer than this is covered by the tests above and
        // by the random chunking below.
        const LONGEST: usize = 96;
        for fixture in FIXTURES.iter().filter(|f| f.stream.len() <= LONGEST) {
            let stream = fixture.stream;
            for first in 0..=stream.len() {
                for second in first..=stream.len() {
                    assert_eq!(
                        parse(&[&stream[..first], &stream[first..second], &stream[second..]]),
                        expected(fixture),
                        "{}: cuts at {first} and {second}",
                        fixture.name
                    );
                }
            }
        }
    }

    /// Every fixture that ends cleanly, one after another, as one stream.
    fn all_together() -> (Vec<u8>, Events) {
        let mut stream = Vec::new();
        let mut events = Vec::new();
        for fixture in FIXTURES {
            let clean_end = fixture.stream.ends_with(b"\n\n")
                || fixture.stream.ends_with(b"\r\r")
                || fixture.stream.ends_with(b"\r\n\r\n");
            // A byte-order mark is only one at the very start.
            if clean_end && !fixture.stream.starts_with(b"\xEF") {
                stream.extend_from_slice(fixture.stream);
                events.extend(expected(fixture));
            }
        }
        (stream, events)
    }

    #[test]
    fn a_long_stream_parses_in_chunks_of_any_sizes() {
        let (stream, events) = all_together();
        assert!(events.len() >= 15, "most fixtures are in the long stream");
        cases::check(
            "sse_chunks",
            200,
            |rng| {
                let mut cuts = Vec::new();
                let mut at = 0;
                while at < stream.len() {
                    at = (at + 1 + cases::small(rng, 40)).min(stream.len());
                    cuts.push(at);
                }
                cuts
            },
            |cuts| {
                let mut chunks = Vec::new();
                let mut from = 0;
                for cut in cuts {
                    chunks.push(&stream[from..*cut]);
                    from = *cut;
                }
                let parsed = parse(&chunks);
                ensure!(parsed == events, "parsed {parsed:?}");
                Ok(())
            },
        );
    }

    #[test]
    fn the_three_line_endings_mean_the_same() {
        let (stream, events) = all_together();
        // Rewrite the stream with one line ending throughout.
        let lines: Vec<&[u8]> = {
            let mut lines = Vec::new();
            let mut rest = stream.as_slice();
            while let Some(end) = rest.iter().position(|b| matches!(b, b'\n' | b'\r')) {
                lines.push(&rest[..end]);
                let crlf = rest[end] == b'\r' && rest.get(end + 1) == Some(&b'\n');
                rest = &rest[end + if crlf { 2 } else { 1 }..];
            }
            lines
        };
        for ending in [&b"\n"[..], b"\r\n", b"\r"] {
            let rewritten = lines.join(ending);
            let mut whole = rewritten.clone();
            whole.extend_from_slice(ending);
            assert_eq!(parse(&[&whole]), events, "line ending {ending:?}");
        }
    }

    #[test]
    fn a_carriage_return_at_the_end_of_a_chunk_is_a_line_end_at_once() {
        let mut parser = SseParser::new();
        parser.push(b"data: x\r");
        assert!(parser.next_event().is_none(), "the event is not closed yet");
        parser.push(b"\r");
        let event = parser.next_event().unwrap().unwrap();
        assert_eq!((event.event, event.data), ("message", "x"));
        // The `\n` that completes the second `\r\n` is not another blank line.
        parser.push(b"\ndata: y\r\n\r\n");
        let event = parser.next_event().unwrap().unwrap();
        assert_eq!(event.data, "y");
        assert!(parser.next_event().is_none());
        assert!(!parser.is_mid_event());
    }

    #[test]
    fn it_says_when_the_stream_stopped_inside_an_event() {
        let mut parser = SseParser::new();
        assert!(!parser.is_mid_event());
        parser.push(b"data: whole\n\n");
        assert!(parser.next_event().is_some());
        assert!(parser.next_event().is_none());
        assert!(!parser.is_mid_event());
        parser.push(b"data: half");
        assert!(parser.next_event().is_none());
        assert!(parser.is_mid_event(), "an unfinished line");
        parser.push(b"\n");
        assert!(parser.next_event().is_none());
        assert!(parser.is_mid_event(), "a field with no blank line after it");
    }

    #[test]
    fn bytes_that_are_not_text_are_replaced_not_refused() {
        let events = parse(&[b"data: ok \xFF\xFE end\n\n"]);
        assert_eq!(
            events,
            [("message".to_owned(), "ok \u{fffd}\u{fffd} end".to_owned())]
        );
    }

    #[test]
    fn an_event_over_the_limit_is_an_error_and_stays_one() {
        // One long line with no end.
        let mut parser = SseParser::with_max_event_len(64);
        parser.push(b"data: ");
        parser.push(&[b'x'; 100]);
        assert_eq!(
            parser.next_event(),
            Some(Err(SseError::EventTooLong { limit: 64 }))
        );
        parser.push(b"\n\n");
        assert!(matches!(parser.next_event(), Some(Err(_))));

        // Many short lines that never reach a blank line.
        let mut parser = SseParser::with_max_event_len(64);
        for _ in 0..20 {
            parser.push(b"data: 0123456789\n");
        }
        assert!(matches!(parser.next_event(), Some(Err(_))));

        // Events under the limit pass, however many there are.
        let mut parser = SseParser::with_max_event_len(64);
        for _ in 0..50 {
            parser.push(b"data: 0123456789\n\n");
            assert!(matches!(parser.next_event(), Some(Ok(_))));
            assert!(parser.next_event().is_none());
        }
    }

    #[test]
    fn memory_is_given_back_as_events_are_read() {
        let mut parser = SseParser::new();
        let event = format!("data: {}\n\n", "x".repeat(1000));
        for _ in 0..200 {
            parser.push(event.as_bytes());
            while parser.next_event().is_some() {}
        }
        assert!(
            parser.buffer.capacity() < 16 * 1024,
            "the buffer kept {} bytes for 1 KB events",
            parser.buffer.capacity()
        );
        // Also when the reader is always one partial line behind.
        let mut parser = SseParser::new();
        for _ in 0..200 {
            parser.push(event.as_bytes());
            parser.push(b"data: par");
            while parser.next_event().is_some() {}
            parser.push(b"tial\n\n");
            while parser.next_event().is_some() {}
        }
        assert!(parser.buffer.capacity() < 64 * 1024);
    }

    /// Bytes that are not a well-formed stream: the pieces of the format in
    /// any order, with bytes that are not text among them.
    fn soup(rng: &mut cortex_testkit::SeededRng) -> Vec<u8> {
        const PIECES: &[&[u8]] = &[
            b"\n",
            b"\n",
            b"\r",
            b"\r\n",
            b":",
            b": ",
            b" ",
            b"data",
            b"data:",
            b"data: ",
            b"event",
            b"event: ",
            b"id: 1",
            b"retry: 5",
            b"x",
            b"{\"a\":1}",
            b"\xEF\xBB\xBF",
            b"\xFF",
            b"\xC3",
            b"\xC3\xA9",
            b"\xF0\x9F\x98\x80",
            b"\0",
        ];
        let mut bytes = Vec::new();
        for _ in 0..cases::small(rng, 60) {
            bytes.extend_from_slice(cases::pick(rng, PIECES));
        }
        bytes
    }

    #[test]
    fn any_bytes_parse_the_same_however_they_are_cut_and_never_panic() {
        // This is the parser's fuzzing: a nightly run multiplies the cases
        // (`docs/testing-and-ci.md`, "Property tests").
        cases::check(
            "sse_soup",
            400,
            |rng| {
                let bytes = soup(rng);
                let mut cuts = Vec::new();
                let mut at = 0;
                while at < bytes.len() {
                    at = (at + 1 + cases::small(rng, 12)).min(bytes.len());
                    cuts.push(at);
                }
                (bytes, cuts)
            },
            |(bytes, cuts)| {
                let whole = parse(&[bytes]);
                let mut chunks = Vec::new();
                let mut from = 0;
                for cut in cuts {
                    chunks.push(&bytes[from..*cut]);
                    from = *cut;
                }
                let cut_up = parse(&chunks);
                ensure!(cut_up == whole, "whole: {whole:?}\n  cut up: {cut_up:?}");
                let single: Vec<&[u8]> = bytes.chunks(1).collect();
                let one_by_one = parse(&single);
                ensure!(one_by_one == whole, "byte by byte: {one_by_one:?}");
                for (event, data) in &whole {
                    ensure!(!event.is_empty(), "an event with no name");
                    ensure!(
                        !event.contains(['\r', '\n']) && !data.contains('\r'),
                        "a line ending inside a field: {event:?} {data:?}"
                    );
                }
                Ok(())
            },
        );
    }
}
