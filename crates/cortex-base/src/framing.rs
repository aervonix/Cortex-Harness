//! Newline-delimited JSON: the framing of the RPC protocol
//! (`docs/architecture/11-rpc-protocol.md`, "Transports").
//!
//! One JSON object per line, UTF-8, terminated by `\n`. A `\r` before the
//! `\n` is tolerated on input and never written. It is the framing MCP uses
//! over standard I/O too, so the RPC server, the MCP client and the
//! extension host share this reader and writer.
//!
//! The pieces:
//!
//! - [`FrameDecoder`] turns bytes into lines. It does no I/O: bytes are
//!   pushed in as they arrive, in chunks of any size, and whole messages are
//!   taken out. That is what lets one decoder serve a blocking pipe, an
//!   async socket and a test that splits the input at every byte.
//! - [`encode`] writes one message as one line.
//! - [`FrameReader`] and [`FrameWriter`] wrap the two around anything that
//!   implements the standard library's `Read` and `Write`.
//!
//! A bad line is an error for that line only. The decoder goes on with the
//! next one, so a peer's mistake does not end the connection.

use std::io::{self, Read, Write};
use std::ops::Range;

use cortex_protocol::jsonrpc::{self, ErrorObject, Message};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The longest line a decoder accepts unless told otherwise: 32 MiB. A
/// message can carry a file or an image, so the limit is generous. It exists
/// so that a peer that never sends a newline cannot use up the memory.
pub const DEFAULT_MAX_LINE_LEN: usize = 32 * 1024 * 1024;

/// How much a [`FrameReader`] asks its source for at a time.
const READ_CHUNK: usize = 32 * 1024;

/// What is wrong with one line, or with the end of the stream.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The line is not JSON.
    #[error("the line is not valid JSON: {0}")]
    InvalidJson(#[source] serde_json::Error),
    /// The line is JSON, and is not a message of the kind being read.
    #[error("the line is JSON but not a valid message: {0}")]
    InvalidMessage(#[source] serde_json::Error),
    /// The line is longer than the decoder's limit. It was dropped.
    #[error("a line is longer than the limit of {limit} bytes and was dropped")]
    TooLong {
        /// The limit, in bytes, not counting the line ending.
        limit: usize,
    },
    /// The stream ended in the middle of a line.
    #[error("the stream ended in the middle of a line, {bytes} bytes after the last newline")]
    Truncated {
        /// How many bytes had arrived since the last newline.
        bytes: usize,
    },
    /// A message could not be written as one line of JSON.
    #[error("the message could not be written as one line of JSON: {0}")]
    Encode(String),
}

impl FrameError {
    /// The JSON-RPC error code that answers this error: a parse error for a
    /// line that is not JSON or never ended, an invalid request for the rest.
    #[must_use]
    pub fn jsonrpc_code(&self) -> i64 {
        match self {
            Self::InvalidJson(_) | Self::Truncated { .. } => jsonrpc::code::PARSE_ERROR,
            Self::InvalidMessage(_) | Self::TooLong { .. } => jsonrpc::code::INVALID_REQUEST,
            Self::Encode(_) => jsonrpc::code::INTERNAL_ERROR,
        }
    }

    /// The error member of the response that tells the peer about this
    /// error. The response has no id: the line's own could not be read.
    #[must_use]
    pub fn to_error_object(&self) -> ErrorObject {
        ErrorObject {
            code: self.jsonrpc_code(),
            message: self.to_string(),
            data: None,
        }
    }
}

/// Reads one line, without its line ending, as a message.
pub fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T, FrameError> {
    serde_json::from_slice(line).map_err(|error| {
        if error.classify() == serde_json::error::Category::Data {
            FrameError::InvalidMessage(error)
        } else {
            FrameError::InvalidJson(error)
        }
    })
}

/// Appends `message` to `out` as one line: compact JSON, then `\n`.
///
/// On an error `out` is left as it was.
pub fn encode<T: Serialize + ?Sized>(message: &T, out: &mut Vec<u8>) -> Result<(), FrameError> {
    let start = out.len();
    if let Err(error) = serde_json::to_writer(&mut *out, message) {
        out.truncate(start);
        return Err(FrameError::Encode(error.to_string()));
    }
    end_line(out, start)
}

/// Ends the line that begins at `start` with a newline, after checking that
/// it has no line break of its own.
///
/// Compact JSON has none: a line break inside a string is written as an
/// escape. The check keeps that true whatever a `Serialize` implementation
/// does (JSON text passed through as it is, for one), because a stray
/// newline would split one message into two.
fn end_line(out: &mut Vec<u8>, start: usize) -> Result<(), FrameError> {
    if out[start..]
        .iter()
        .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        out.truncate(start);
        return Err(FrameError::Encode(
            "the JSON contains a line break outside a string".to_owned(),
        ));
    }
    out.push(b'\n');
    Ok(())
}

/// Splits a stream of bytes into lines and reads each as a message.
///
/// Push bytes in with [`push`](Self::push) as they arrive, then take
/// messages out with [`next_message`](Self::next_message) until it returns
/// `None`. How the bytes were divided into chunks never changes what comes
/// out.
///
/// Empty lines are skipped. A line longer than the limit is reported once
/// and dropped, without being kept in memory.
#[derive(Debug)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    /// Where the first line not yet taken begins.
    start: usize,
    /// Everything before this index has been searched for a newline.
    searched: usize,
    max_line_len: usize,
    /// Set while the rest of an over-long line is being thrown away.
    dropping: bool,
    /// How many bytes of the line being dropped have gone by.
    dropped: usize,
}

impl FrameDecoder {
    /// A decoder with the default line limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_line_len(DEFAULT_MAX_LINE_LEN)
    }

    /// A decoder that refuses lines longer than `max_line_len` bytes, not
    /// counting the line ending.
    #[must_use]
    pub fn with_max_line_len(max_line_len: usize) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            searched: 0,
            max_line_len,
            dropping: false,
            dropped: 0,
        }
    }

    /// Adds bytes that arrived from the peer.
    pub fn push(&mut self, mut bytes: &[u8]) {
        if self.dropping {
            match bytes.iter().position(|&byte| byte == b'\n') {
                Some(newline) => {
                    self.dropping = false;
                    self.dropped = 0;
                    bytes = &bytes[newline + 1..];
                }
                None => {
                    self.dropped = self.dropped.saturating_add(bytes.len());
                    return;
                }
            }
        }
        // Make room by dropping the lines already taken, once they are at
        // least half of what is held, so that the copying stays linear.
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
            self.searched = 0;
        } else if self.start >= self.buffer.len() / 2 {
            self.buffer.drain(..self.start);
            self.searched -= self.start;
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole line, without its line ending. `None` when no whole
    /// line is buffered: push more bytes.
    pub fn next_line(&mut self) -> Option<Result<&[u8], FrameError>> {
        match self.take_line()? {
            Ok(line) => Some(Ok(&self.buffer[line])),
            Err(error) => Some(Err(error)),
        }
    }

    /// The next message. `None` when no whole line is buffered: push more
    /// bytes. After an error the next call goes on with the next line.
    pub fn next_message<T: DeserializeOwned>(&mut self) -> Option<Result<T, FrameError>> {
        match self.take_line()? {
            Ok(line) => Some(decode(&self.buffer[line])),
            Err(error) => Some(Err(error)),
        }
    }

    /// Tells the decoder that the stream has ended. An error if it ended in
    /// the middle of a line: every message ends with a newline, so those
    /// bytes are a message that was cut off.
    pub fn finish(&mut self) -> Result<(), FrameError> {
        let bytes = if self.dropping {
            self.dropped
        } else {
            self.buffer.len() - self.start
        };
        self.buffer.clear();
        self.start = 0;
        self.searched = 0;
        self.dropping = false;
        self.dropped = 0;
        if bytes == 0 {
            Ok(())
        } else {
            Err(FrameError::Truncated { bytes })
        }
    }

    /// How many bytes are held that have not yet been taken as a line.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buffer.len() - self.start
    }

    /// Finds the next line that is not empty and marks it taken.
    fn take_line(&mut self) -> Option<Result<Range<usize>, FrameError>> {
        loop {
            let newline = self.buffer[self.searched..]
                .iter()
                .position(|&byte| byte == b'\n');
            let Some(offset) = newline else {
                self.searched = self.buffer.len();
                // One byte over the limit may be the `\r` of a line that is
                // exactly at the limit.
                if self.buffered() > self.max_line_len.saturating_add(1) {
                    self.dropped = self.buffered();
                    self.buffer.clear();
                    self.start = 0;
                    self.searched = 0;
                    self.dropping = true;
                    return Some(Err(FrameError::TooLong {
                        limit: self.max_line_len,
                    }));
                }
                return None;
            };
            let begin = self.start;
            let mut end = self.searched + offset;
            self.start = end + 1;
            self.searched = self.start;
            if end > begin && self.buffer[end - 1] == b'\r' {
                end -= 1;
            }
            if end - begin > self.max_line_len {
                return Some(Err(FrameError::TooLong {
                    limit: self.max_line_len,
                }));
            }
            if end > begin {
                return Some(Ok(begin..end));
            }
        }
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// An error from a [`FrameReader`] or a [`FrameWriter`].
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// Reading or writing the underlying stream failed. The stream is
    /// probably gone.
    #[error("the stream failed: {0}")]
    Io(#[from] io::Error),
    /// One line was bad. Reading can go on.
    #[error(transparent)]
    Frame(#[from] FrameError),
}

/// Reads messages from a blocking byte stream, such as standard input or a
/// child process's output.
#[derive(Debug)]
pub struct FrameReader<R> {
    source: R,
    decoder: FrameDecoder,
    chunk: Box<[u8]>,
    ended: bool,
}

impl<R: Read> FrameReader<R> {
    /// A reader with the default line limit.
    pub fn new(source: R) -> Self {
        Self::with_decoder(source, FrameDecoder::new())
    }

    /// A reader that uses `decoder`, for a limit other than the default.
    pub fn with_decoder(source: R, decoder: FrameDecoder) -> Self {
        Self {
            source,
            decoder,
            chunk: vec![0; READ_CHUNK].into_boxed_slice(),
            ended: false,
        }
    }

    /// The next message, waiting for it if need be. `Ok(None)` at the end of
    /// the stream.
    ///
    /// After [`StreamError::Frame`] the next call reads the next line.
    pub fn read_message<T: DeserializeOwned>(&mut self) -> Result<Option<T>, StreamError> {
        loop {
            if let Some(message) = self.decoder.next_message() {
                return Ok(Some(message?));
            }
            if self.ended {
                return Ok(None);
            }
            match self.source.read(&mut self.chunk) {
                Ok(0) => {
                    self.ended = true;
                    self.decoder.finish()?;
                }
                Ok(count) => self.decoder.push(&self.chunk[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// The next JSON-RPC message.
    pub fn read(&mut self) -> Result<Option<Message>, StreamError> {
        self.read_message()
    }

    /// Gives the stream back. Bytes already read from it and not yet taken
    /// as messages are lost.
    pub fn into_inner(self) -> R {
        self.source
    }
}

/// Writes messages to a blocking byte stream, one line each, flushing after
/// every message so that a peer that follows along sees it at once.
#[derive(Debug)]
pub struct FrameWriter<W> {
    sink: W,
    line: Vec<u8>,
}

impl<W: Write> FrameWriter<W> {
    /// A writer onto `sink`.
    pub fn new(sink: W) -> Self {
        Self {
            sink,
            line: Vec::new(),
        }
    }

    /// Writes one message as one line, in a single write, and flushes.
    pub fn write_message<T: Serialize + ?Sized>(&mut self, message: &T) -> Result<(), StreamError> {
        self.line.clear();
        encode(message, &mut self.line)?;
        self.sink.write_all(&self.line)?;
        self.sink.flush()?;
        Ok(())
    }

    /// Writes one JSON-RPC message.
    pub fn write(&mut self, message: &Message) -> Result<(), StreamError> {
        self.write_message(message)
    }

    /// Gives the stream back.
    pub fn into_inner(self) -> W {
        self.sink
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::jsonrpc::{Notification, Request, RequestId, Response};
    use serde_json::{Value, json};

    use super::*;
    use crate::cases::{self, ensure};
    use crate::rng::Rng;

    /// The protocol's golden transcript: every method and event of version 1.
    const GOLDEN: &str = include_str!("../../cortex-protocol/tests/golden/v1-session.jsonl");

    /// A short stream with what makes framing hard: characters of two, three
    /// and four bytes, escaped line breaks inside strings, empty lines, and
    /// all three kinds of message.
    fn sample_messages() -> Vec<Message> {
        vec![
            Request {
                id: RequestId::Number(1),
                method: "turn/start".to_owned(),
                params: Some(json!({"text": "naïve café, 日本語, 🙂 and a\nline break\r\n"})),
            }
            .into(),
            Notification {
                method: "session/event".to_owned(),
                params: Some(
                    json!({"type": "item.delta", "delta": {"text": "\\n is not a newline"}}),
                ),
            }
            .into(),
            Response {
                id: Some(RequestId::String("a-1".to_owned())),
                result: Ok(json!({})),
            }
            .into(),
            Notification {
                method: "tick".to_owned(),
                params: None,
            }
            .into(),
        ]
    }

    /// The sample as a byte stream with the given line ending, and with empty
    /// lines mixed in.
    fn sample_stream(ending: &str) -> Vec<u8> {
        stream_of(&sample_messages(), ending)
    }

    /// A shorter stream with the same difficulties, for the test whose work
    /// grows with the square of the stream's length.
    fn short_messages() -> Vec<Message> {
        vec![
            Notification {
                method: "a".to_owned(),
                params: Some(json!(["é日🙂", "\n\r\n"])),
            }
            .into(),
            Response {
                id: Some(RequestId::Number(7)),
                result: Ok(json!({})),
            }
            .into(),
            Request {
                id: RequestId::String("b".to_owned()),
                method: "c".to_owned(),
                params: None,
            }
            .into(),
        ]
    }

    fn stream_of(messages: &[Message], ending: &str) -> Vec<u8> {
        let mut stream = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            if index == 1 {
                stream.extend_from_slice(ending.as_bytes());
            }
            let mut line = Vec::new();
            encode(message, &mut line).unwrap();
            assert_eq!(line.pop(), Some(b'\n'));
            stream.extend_from_slice(&line);
            stream.extend_from_slice(ending.as_bytes());
        }
        stream.extend_from_slice(ending.as_bytes());
        stream
    }

    fn drain_messages(decoder: &mut FrameDecoder, out: &mut Vec<Message>) {
        while let Some(message) = decoder.next_message() {
            out.push(message.unwrap());
        }
    }

    fn drain_lines(decoder: &mut FrameDecoder, out: &mut Vec<Vec<u8>>) {
        while let Some(line) = decoder.next_line() {
            out.push(line.unwrap().to_vec());
        }
    }

    #[test]
    fn every_split_into_two_chunks_gives_the_same_messages() {
        let expected = sample_messages();
        for ending in ["\n", "\r\n"] {
            let stream = sample_stream(ending);
            for split in 0..=stream.len() {
                let mut decoder = FrameDecoder::new();
                let mut messages = Vec::new();
                decoder.push(&stream[..split]);
                drain_messages(&mut decoder, &mut messages);
                decoder.push(&stream[split..]);
                drain_messages(&mut decoder, &mut messages);
                assert_eq!(messages, expected, "split at {split}, ending {ending:?}");
                assert!(decoder.finish().is_ok());
            }
        }
    }

    #[test]
    fn every_split_into_three_chunks_gives_the_same_messages() {
        let expected = short_messages();
        for ending in ["\n", "\r\n"] {
            let stream = stream_of(&expected, ending);
            assert!(stream.len() < 160, "kept short: {} bytes", stream.len());
            // Every first cut, with every second cut after it in the
            // nightly run and every third one on a pull request: which
            // third depends on the first, so every place is a second cut
            // for some first one.
            let step = if cases::exhaustive() { 1 } else { 3 };
            for first in 0..=stream.len() {
                for second in (first..=stream.len()).step_by(step) {
                    let mut decoder = FrameDecoder::new();
                    let mut messages = Vec::new();
                    for chunk in [&stream[..first], &stream[first..second], &stream[second..]] {
                        decoder.push(chunk);
                        drain_messages(&mut decoder, &mut messages);
                    }
                    assert_eq!(messages, expected, "split at {first} and {second}");
                }
            }
        }
    }

    #[test]
    fn every_chunk_size_gives_the_same_messages_whether_drained_early_or_late() {
        let expected = sample_messages();
        for ending in ["\n", "\r\n"] {
            let stream = sample_stream(ending);
            for size in 1..=stream.len() {
                let mut eager = FrameDecoder::new();
                let mut lazy = FrameDecoder::new();
                let mut messages = Vec::new();
                for chunk in stream.chunks(size) {
                    eager.push(chunk);
                    drain_messages(&mut eager, &mut messages);
                    lazy.push(chunk);
                }
                assert_eq!(messages, expected, "chunks of {size}");
                messages.clear();
                drain_messages(&mut lazy, &mut messages);
                assert_eq!(messages, expected, "chunks of {size}, drained at the end");
            }
        }
    }

    #[test]
    fn the_golden_transcript_survives_every_split_of_its_start() {
        // Every split of its first kilobyte, with both line endings,
        // compared line by line: of its first three hundred bytes on a pull
        // request, which is more than its first message, and of the whole
        // kilobyte in the nightly run. The whole transcript is covered by
        // the next two tests.
        let expected: Vec<&str> = GOLDEN.lines().collect();
        let most = if cases::exhaustive() { 1024 } else { 300 };
        for ending in ["\n", "\r\n"] {
            let stream = GOLDEN.replace('\n', ending).into_bytes();
            let window = stream.len().min(most);
            for split in 0..=window {
                let mut decoder = FrameDecoder::new();
                let mut lines = Vec::new();
                decoder.push(&stream[..split]);
                drain_lines(&mut decoder, &mut lines);
                decoder.push(&stream[split..]);
                drain_lines(&mut decoder, &mut lines);
                assert_eq!(lines.len(), expected.len(), "split at {split}");
                assert!(
                    lines
                        .iter()
                        .zip(&expected)
                        .all(|(got, want)| got == want.as_bytes()),
                    "split at {split}, ending {ending:?}"
                );
            }
        }
    }

    #[test]
    fn the_golden_transcript_survives_a_byte_at_a_time() {
        let expected: Vec<Message> = GOLDEN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(expected.len() > 50, "the transcript is there");
        for ending in ["\n", "\r\n"] {
            let stream = GOLDEN.replace('\n', ending).into_bytes();
            let mut decoder = FrameDecoder::new();
            let mut messages = Vec::new();
            for byte in &stream {
                decoder.push(std::slice::from_ref(byte));
                drain_messages(&mut decoder, &mut messages);
                assert!(decoder.buffered() < 4096, "taken lines are let go");
            }
            assert_eq!(messages, expected);
            assert!(decoder.finish().is_ok());
        }
    }

    #[test]
    fn property_any_chunking_of_the_golden_transcript_gives_the_same_messages() {
        let expected: Vec<Message> = GOLDEN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let crlf = GOLDEN.replace('\n', "\r\n");
        cases::check(
            "framing_chunks",
            20,
            |rng| {
                let use_crlf = rng.below(2) == 1;
                let length = if use_crlf { crlf.len() } else { GOLDEN.len() };
                let largest = *cases::pick(rng, &[2u64, 7, 64, 700, 9000]);
                let mut cuts = vec![0usize];
                while cuts[cuts.len() - 1] < length {
                    let step = 1 + rng.below(largest) as usize;
                    cuts.push((cuts[cuts.len() - 1] + step).min(length));
                }
                (use_crlf, cuts)
            },
            |(use_crlf, cuts)| {
                let stream = if *use_crlf {
                    crlf.as_bytes()
                } else {
                    GOLDEN.as_bytes()
                };
                let mut decoder = FrameDecoder::new();
                let mut messages = Vec::new();
                for pair in cuts.windows(2) {
                    decoder.push(&stream[pair[0]..pair[1]]);
                    while let Some(message) = decoder.next_message::<Message>() {
                        messages.push(message?);
                    }
                }
                decoder.finish()?;
                ensure!(messages == expected, "the messages differ");
                Ok(())
            },
        );
    }

    #[test]
    fn a_carriage_return_is_dropped_only_before_the_newline() {
        let mut decoder = FrameDecoder::new();
        decoder.push(b"{\"a\":1}\r\n{\"b\":2}\n\r\n\n{\"c\":3}\r\r\n");
        assert_eq!(decoder.next_line().unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(decoder.next_line().unwrap().unwrap(), b"{\"b\":2}");
        // Only one `\r` belongs to the line ending. The other is content,
        // and makes the line bad JSON for whoever reads it as a message.
        assert_eq!(decoder.next_line().unwrap().unwrap(), b"{\"c\":3}\r");
        assert!(decoder.next_line().is_none());

        decoder.push(b"{\"a\":\r1}\n");
        assert!(
            matches!(
                decoder.next_message::<Value>(),
                Some(Ok(value)) if value == json!({"a": 1})
            ),
            "a carriage return between tokens is JSON whitespace"
        );
        decoder.push(b"{\"a\":\"x\ry\"}\n");
        assert!(
            matches!(
                decoder.next_message::<Value>(),
                Some(Err(FrameError::InvalidJson(_)))
            ),
            "a raw carriage return inside a string is not JSON"
        );
    }

    #[test]
    fn a_bad_line_is_an_error_for_that_line_only() {
        let mut decoder = FrameDecoder::new();
        decoder.push(
            b"{\"jsonrpc\":\"2.0\",\"method\":\"a\"}\n\
              {not json}\n\
              [1,2,3]\n\
              {\"jsonrpc\":\"2.0\"}\n\
              \"text\"\n\
              {\"jsonrpc\":\"2.0\",\"method\":\"b\"} trailing\n\
              \xff\xfe\n\
              {\"jsonrpc\":\"2.0\",\"method\":\"c\"}\n",
        );
        let mut outcomes = Vec::new();
        while let Some(message) = decoder.next_message::<Message>() {
            outcomes.push(match message {
                Ok(Message::Notification(notification)) => notification.method,
                Ok(other) => panic!("unexpected {other:?}"),
                Err(error) => {
                    let object = error.to_error_object();
                    assert_eq!(object.code, error.jsonrpc_code());
                    assert!(!object.message.is_empty());
                    object.code.to_string()
                }
            });
        }
        assert_eq!(
            outcomes,
            [
                "a", "-32700", "-32600", "-32600", "-32600", "-32700", "-32700", "c"
            ]
        );
    }

    #[test]
    fn a_line_at_the_limit_passes_and_one_byte_more_does_not() {
        for ending in ["\n", "\r\n"] {
            let at_limit = format!("\"{}\"", "x".repeat(14));
            assert_eq!(at_limit.len(), 16);
            let over = format!("\"{}\"", "x".repeat(15));

            let mut decoder = FrameDecoder::with_max_line_len(16);
            decoder.push(format!("{at_limit}{ending}{over}{ending}{at_limit}{ending}").as_bytes());
            assert!(matches!(decoder.next_message::<String>(), Some(Ok(text)) if text.len() == 14));
            assert!(matches!(
                decoder.next_message::<String>(),
                Some(Err(FrameError::TooLong { limit: 16 }))
            ));
            assert!(matches!(decoder.next_message::<String>(), Some(Ok(_))));
            assert!(decoder.next_message::<String>().is_none());

            // The same, a byte at a time: the line at the limit must not be
            // refused while its line ending is still on the way.
            let mut decoder = FrameDecoder::with_max_line_len(16);
            let mut read = Vec::new();
            for byte in format!("{at_limit}{ending}{at_limit}{ending}").as_bytes() {
                decoder.push(std::slice::from_ref(byte));
                while let Some(message) = decoder.next_message::<String>() {
                    read.push(message.unwrap());
                }
            }
            assert_eq!(read.len(), 2, "ending {ending:?}");
        }
    }

    #[test]
    fn an_endless_line_is_reported_once_and_not_kept() {
        let mut decoder = FrameDecoder::with_max_line_len(64);
        let mut errors = 0;
        for _ in 0..10_000 {
            decoder.push(&[b'x'; 100]);
            while let Some(line) = decoder.next_line() {
                assert!(matches!(line, Err(FrameError::TooLong { limit: 64 })));
                errors += 1;
            }
            assert!(decoder.buffered() <= 100, "the line is not being kept");
        }
        assert_eq!(errors, 1);

        // The stream recovers at the next newline.
        decoder.push(b"xxx\n{\"ok\":true}\n");
        assert!(matches!(
            decoder.next_message::<Value>(),
            Some(Ok(value)) if value == json!({"ok": true})
        ));
        assert!(decoder.next_line().is_none());
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn a_stream_that_ends_inside_a_line_is_truncated() {
        let mut decoder = FrameDecoder::new();
        assert!(decoder.finish().is_ok(), "an empty stream is fine");

        decoder.push(b"{\"a\":1}\n{\"b\":");
        assert!(decoder.next_line().unwrap().is_ok());
        assert!(decoder.next_line().is_none());
        assert!(matches!(
            decoder.finish(),
            Err(FrameError::Truncated { bytes: 5 })
        ));
        assert_eq!(decoder.buffered(), 0);
        assert!(decoder.finish().is_ok(), "reported once");

        let mut decoder = FrameDecoder::with_max_line_len(4);
        decoder.push(b"0123456789");
        assert!(matches!(
            decoder.next_line(),
            Some(Err(FrameError::TooLong { .. }))
        ));
        decoder.push(b"01234");
        assert!(matches!(
            decoder.finish(),
            Err(FrameError::Truncated { bytes: 15 })
        ));

        let error = FrameError::Truncated { bytes: 5 };
        assert_eq!(error.jsonrpc_code(), jsonrpc::code::PARSE_ERROR);
    }

    #[test]
    fn a_message_is_written_as_exactly_one_line() {
        let mut out = b"kept".to_vec();
        for message in sample_messages() {
            encode(&message, &mut out).unwrap();
        }
        assert!(out.starts_with(b"kept{"));
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches('\n').count(), sample_messages().len());
        assert!(text.ends_with("}\n"));
        assert!(!text.contains('\r'), "line breaks in strings are escaped");
        assert!(text.contains("\\n") && text.contains("\\r"));
        assert!(
            text.contains("日本語") && text.contains("🙂"),
            "UTF-8 as is"
        );
    }

    #[test]
    fn a_value_that_cannot_be_one_line_is_refused_and_leaves_no_trace() {
        /// Refuses to be written.
        struct Unwritable;
        impl Serialize for Unwritable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                use serde::ser::Error;
                Err(S::Error::custom("cannot be written"))
            }
        }
        let mut out = b"before\n".to_vec();
        let error = encode(&Unwritable, &mut out).unwrap_err();
        assert!(matches!(error, FrameError::Encode(_)));
        assert_eq!(error.jsonrpc_code(), jsonrpc::code::INTERNAL_ERROR);
        assert_eq!(out, b"before\n");

        // A map whose keys are not strings is the common real case.
        let bad: std::collections::BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into();
        assert!(encode(&bad, &mut out).is_err());
        assert_eq!(out, b"before\n");

        // JSON that is spread over two lines is not a line.
        for broken in ["{\"a\":\n1}", "{\"a\":1}\r"] {
            let start = out.len();
            out.extend_from_slice(broken.as_bytes());
            assert!(matches!(
                end_line(&mut out, start),
                Err(FrameError::Encode(_))
            ));
            assert_eq!(out, b"before\n");
        }
    }

    /// Gives its bytes out a few at a time, with an interrupted read now and
    /// then, as a pipe may.
    struct Trickle<'a> {
        bytes: &'a [u8],
        step: usize,
        reads: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads.is_multiple_of(5) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = self.step.min(self.bytes.len()).min(buffer.len());
            buffer[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }

    #[test]
    fn what_the_writer_writes_the_reader_reads() {
        let expected: Vec<Message> = GOLDEN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut writer = FrameWriter::new(Vec::new());
        for message in &expected {
            writer.write(message).unwrap();
        }
        let written = writer.into_inner();
        // Not byte for byte the transcript: an envelope carries its
        // parameters as untyped JSON, whose keys are written in sorted
        // order. Line for line it is the same messages.
        let lines: Vec<&[u8]> = written.split_inclusive(|&byte| byte == b'\n').collect();
        assert_eq!(lines.len(), expected.len());
        for (line, message) in lines.iter().zip(&expected) {
            assert_eq!(line.last(), Some(&b'\n'));
            assert_eq!(
                &decode::<Message>(&line[..line.len() - 1]).unwrap(),
                message
            );
        }

        for step in [1, 3, 1000, usize::MAX] {
            let mut reader = FrameReader::new(Trickle {
                bytes: &written,
                step,
                reads: 0,
            });
            let mut messages = Vec::new();
            while let Some(message) = reader.read().unwrap() {
                messages.push(message);
            }
            assert_eq!(messages, expected, "{step} bytes at a time");
            assert!(reader.read().unwrap().is_none(), "the end stays the end");
        }
    }

    #[test]
    fn the_reader_reports_a_bad_line_and_goes_on() {
        let stream = b"{\"jsonrpc\":\"2.0\",\"method\":\"a\"}\r\nnot json\n{\"jsonrpc\":\"2.0\",\"method\":\"b\"}\n{\"cut";
        let mut reader =
            FrameReader::with_decoder(&stream[..], FrameDecoder::with_max_line_len(1024));
        assert!(matches!(reader.read(), Ok(Some(Message::Notification(n))) if n.method == "a"));
        assert!(matches!(
            reader.read(),
            Err(StreamError::Frame(FrameError::InvalidJson(_)))
        ));
        assert!(matches!(reader.read(), Ok(Some(Message::Notification(n))) if n.method == "b"));
        assert!(matches!(
            reader.read(),
            Err(StreamError::Frame(FrameError::Truncated { bytes: 5 }))
        ));
        assert!(matches!(reader.read(), Ok(None)));
        let _stream = reader.into_inner();
    }

    /// Fails every read and write.
    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_stream_is_an_io_error() {
        let mut reader = FrameReader::new(Broken);
        assert!(matches!(
            reader.read(),
            Err(StreamError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe
        ));
        let mut writer = FrameWriter::new(Broken);
        let error = writer.write_message(&json!({"a": 1})).unwrap_err();
        assert!(matches!(error, StreamError::Io(_)));
        assert!(error.to_string().contains("the stream failed"));
    }

    /// Counts writes and flushes.
    #[derive(Default)]
    struct Counting {
        writes: usize,
        flushes: usize,
        bytes: Vec<u8>,
    }

    impl Write for Counting {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn each_message_is_one_write_and_one_flush() {
        let mut writer = FrameWriter::new(Counting::default());
        for message in sample_messages() {
            writer.write(&message).unwrap();
        }
        let sink = writer.into_inner();
        assert_eq!(sink.writes, 4);
        assert_eq!(sink.flushes, 4);
        assert_eq!(
            sink.bytes,
            sample_stream("\n")
                .iter()
                .fold(Vec::new(), |mut out, &byte| {
                    // The sample stream has empty lines mixed in. The writer has none.
                    if !(byte == b'\n' && out.last().is_none_or(|&last| last == b'\n')) {
                        out.push(byte);
                    }
                    out
                })
        );
    }
}
