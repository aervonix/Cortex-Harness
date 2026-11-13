//! Recorded provider responses: the files a fixture is made of, and what is
//! taken out of a recording before it may be written
//! (`docs/testing-and-ci.md`, "Fixtures").
//!
//! # A fixture
//!
//! A directory under a protocol's `fixtures/` directory, with four files:
//!
//! | File | What it holds |
//! |---|---|
//! | `request.json` | The body of the request, as it was sent. |
//! | `response.sse` | The response body, byte for byte: for a stream, the events as the provider sent them. |
//! | `response.json` | The status, and the response headers that say something about the response: the content type, `retry-after`, the rate limits. |
//! | `note.md` | What the fixture shows, the model, and when and how it was made. |
//!
//! A fixture is either **recorded** from a real provider by `cargo xtask
//! record`, by hand, or **written by hand** from the provider's documented
//! format, for the cases that cannot be provoked. Its note says which.
//!
//! # What is taken out of a recording
//!
//! [`Scrubber`] is what stands between a real response and the repository.
//!
//! - **Credentials never reach it.** A recording holds no request header at
//!   all, and of the response headers only the ones on a short list
//!   ([`keeps_header`]). Everything else is dropped: the ids of the
//!   organisation and the workspace, cookies, whatever a proxy added.
//! - **Identifiers are replaced**, each by a stand-in that is the same
//!   wherever the identifier appears, so that a tool result still names its
//!   call: the provider's ids for messages, requests, tool calls and the
//!   like (`msg_…`, `req_…`, `toolu_…`), and the opaque payloads of
//!   reasoning blocks (`signature`, `data`, `encrypted_content`), which are
//!   bound to the account that produced them.
//! - **The key is looked for in everything that is about to be written or
//!   printed** ([`Scrubber::check`]). If the key, or any twelve characters
//!   in a row of it, is there, nothing is written and nothing is printed:
//!   the recording is refused.

#![allow(
    clippy::disallowed_methods,
    reason = "test support: fixture files are read and written on the test's own thread"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cortex_base::Secret;
use cortex_llm::Headers;
use serde_json::{Value, json};

/// The response headers a fixture keeps. Every other header is dropped.
///
/// It is a list of what is kept, not of what is removed, so that a header
/// nobody thought of is left out.
#[must_use]
pub fn keeps_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(name.as_str(), "content-type" | "retry-after" | "request-id")
        || name.starts_with("anthropic-ratelimit-")
        || name.starts_with("anthropic-fast-")
        || name.starts_with("anthropic-priority-")
}

/// The prefixes of the identifiers a provider makes up, each followed by an
/// underscore and letters and digits.
const ID_PREFIXES: &[&str] = &[
    "msg", "req", "toolu", "srvtoolu", "org", "wrkspc", "user", "file", "container", "batch",
    "msgbatch", "resp", "call", "chatcmpl",
];

/// The members whose values are opaque payloads tied to an account.
const OPAQUE_MEMBERS: &[&str] = &["signature", "data", "encrypted_content", "encrypted_index"];

/// How much of a key is enough to refuse a recording: this many of its
/// characters in a row.
const KEY_WINDOW: usize = 12;

/// A recording was refused, or a fixture could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureError {
    /// What happened, for a person. Never the key.
    pub message: String,
}

impl std::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FixtureError {}

fn problem(message: impl Into<String>) -> FixtureError {
    FixtureError {
        message: message.into(),
    }
}

/// One recorded exchange with a provider. See the [module](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    /// The request body.
    pub request: Vec<u8>,
    /// The response's status.
    pub status: u16,
    /// The response headers that were kept.
    pub headers: Headers,
    /// The response body, byte for byte.
    pub response: Vec<u8>,
    /// What the fixture shows, and how it was made.
    pub note: String,
}

impl Fixture {
    /// Reads the fixture in `directory`.
    pub fn load(directory: &Path) -> Result<Self, FixtureError> {
        let read = |name: &str| {
            let path = directory.join(name);
            std::fs::read(&path)
                .map_err(|error| problem(format!("cannot read {}: {error}", path.display())))
        };
        let meta: Value = serde_json::from_slice(&read("response.json")?).map_err(|error| {
            problem(format!(
                "{}: response.json is not JSON: {error}",
                directory.display()
            ))
        })?;
        let status = meta["status"]
            .as_u64()
            .and_then(|status| u16::try_from(status).ok())
            .ok_or_else(|| problem(format!("{}: no status", directory.display())))?;
        let mut headers = Headers::new();
        if let Some(listed) = meta["headers"].as_object() {
            for (name, value) in listed {
                headers.insert(name, value.as_str().unwrap_or_default());
            }
        }
        let note = String::from_utf8_lossy(&read("note.md")?).into_owned();
        Ok(Self {
            request: read("request.json")?,
            status,
            headers,
            response: read("response.sse")?,
            note,
        })
    }

    /// Writes the fixture's four files into `directory`, which is made if
    /// it is not there.
    pub fn write(&self, directory: &Path) -> Result<(), FixtureError> {
        std::fs::create_dir_all(directory)
            .map_err(|error| problem(format!("cannot make {}: {error}", directory.display())))?;
        let headers: BTreeMap<&str, &str> = self.headers.iter().collect();
        let mut meta = serde_json::to_string_pretty(&json!({
            "status": self.status,
            "headers": headers,
        }))
        .map_err(|error| problem(format!("cannot write the headers: {error}")))?;
        meta.push('\n');
        let files: [(&str, &[u8]); 4] = [
            ("request.json", &self.request),
            ("response.sse", &self.response),
            ("response.json", meta.as_bytes()),
            ("note.md", self.note.as_bytes()),
        ];
        for (name, bytes) in files {
            let path = directory.join(name);
            std::fs::write(&path, bytes)
                .map_err(|error| problem(format!("cannot write {}: {error}", path.display())))?;
        }
        Ok(())
    }
}

/// The directory of a crate's fixtures for one protocol:
/// `<crate>/fixtures/<protocol>`.
#[must_use]
pub fn directory(manifest_dir: &str, protocol: &str) -> PathBuf {
    Path::new(manifest_dir).join("fixtures").join(protocol)
}

/// Takes credentials and identifiers out of a recording, and refuses to let
/// the key through. See the [module](self).
#[derive(Debug)]
pub struct Scrubber {
    key: Secret,
    /// The stand-in for each identifier seen so far.
    ids: BTreeMap<String, String>,
    /// The stand-in for each opaque payload seen so far.
    opaque: BTreeMap<String, String>,
}

impl Scrubber {
    /// A scrubber that guards `key`.
    #[must_use]
    pub fn new(key: Secret) -> Self {
        Self {
            key,
            ids: BTreeMap::new(),
            opaque: BTreeMap::new(),
        }
    }

    /// Fails if `bytes` hold the key or any [`KEY_WINDOW`] characters in a
    /// row of it. `what` names the thing that was about to be written or
    /// printed. The error never holds the key.
    pub fn check(&self, what: &str, bytes: &[u8]) -> Result<(), FixtureError> {
        let key = self.key.expose().as_bytes();
        if key.is_empty() {
            return Err(problem(
                "the key is empty, so nothing can be checked against it",
            ));
        }
        let window = KEY_WINDOW.min(key.len());
        let found = key
            .windows(window)
            .any(|part| bytes.windows(window).any(|candidate| candidate == part));
        if found {
            return Err(problem(format!(
                "refused: {what} holds the API key, or part of it. Nothing was written."
            )));
        }
        Ok(())
    }

    /// Text that may be printed: `text` itself, after [`check`](Self::check).
    pub fn printable<'a>(&self, text: &'a str) -> Result<&'a str, FixtureError> {
        self.check("the output", text.as_bytes())?;
        Ok(text)
    }

    /// The response headers that are kept, with the request id replaced.
    #[must_use]
    pub fn headers(&mut self, headers: &Headers) -> Headers {
        let mut kept = Headers::new();
        for (name, value) in headers.iter() {
            if keeps_header(name) {
                let value = self.text(value);
                kept.insert(name, value);
            }
        }
        kept
    }

    /// A request or response body with its identifiers and opaque payloads
    /// replaced. Bytes that are not text are returned as they are: there is
    /// nothing in them this can read, and [`check`](Self::check) still
    /// looks at them.
    #[must_use]
    pub fn body(&mut self, bytes: &[u8]) -> Vec<u8> {
        match std::str::from_utf8(bytes) {
            Ok(text) => self.text(text).into_bytes(),
            Err(_) => bytes.to_vec(),
        }
    }

    /// Text with its identifiers and opaque payloads replaced.
    #[must_use]
    pub fn text(&mut self, text: &str) -> String {
        let text = self.replace_opaque(text);
        self.replace_ids(&text)
    }

    /// Replaces each `prefix_letters-and-digits` identifier.
    fn replace_ids(&mut self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        while at < bytes.len() {
            let boundary = at == 0 || !is_id_byte(bytes[at - 1]);
            let matched = boundary
                .then(|| {
                    ID_PREFIXES.iter().find_map(|prefix| {
                        let rest = text[at..].strip_prefix(prefix)?.strip_prefix('_')?;
                        let length = rest.bytes().take_while(|byte| is_id_byte(*byte)).count();
                        // A real identifier is long. A short word after the
                        // underscore is a name, such as `user_id`.
                        (length >= 8).then_some((*prefix, prefix.len() + 1 + length))
                    })
                })
                .flatten();
            match matched {
                Some((prefix, length)) => {
                    let id = &text[at..at + length];
                    let next = self.ids.len() + 1;
                    let stand_in = self
                        .ids
                        .entry(id.to_owned())
                        .or_insert_with(|| format!("{prefix}_fixture{next:04}"));
                    out.push_str(stand_in);
                    at += length;
                }
                None => {
                    // One whole character.
                    let width = text[at..].chars().next().map_or(1, char::len_utf8);
                    out.push_str(&text[at..at + width]);
                    at += width;
                }
            }
        }
        out
    }

    /// Replaces the string value of each opaque member: `"signature": "…"`.
    fn replace_opaque(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        'scan: while !rest.is_empty() {
            for member in OPAQUE_MEMBERS {
                // The member as a key, in a body or inside an event's data.
                let Some(after_key) = rest
                    .strip_prefix('"')
                    .and_then(|rest| rest.strip_prefix(member))
                    .and_then(|rest| rest.strip_prefix('"'))
                else {
                    continue;
                };
                let after_colon = after_key.trim_start();
                let Some(after_colon) = after_colon.strip_prefix(':') else {
                    continue;
                };
                let value = after_colon.trim_start();
                let Some(value) = value.strip_prefix('"') else {
                    continue;
                };
                // A payload is base64: it has no quote and no escape.
                let Some(end) = value.find('"') else { continue };
                let payload = &value[..end];
                if payload.len() < 16 || payload.contains('\\') {
                    continue;
                }
                let next = self.opaque.len() + 1;
                let stand_in = self
                    .opaque
                    .entry(payload.to_owned())
                    .or_insert_with(|| format!("fixture-{member}-{next:02}"));
                let consumed = rest.len() - value.len();
                out.push_str(&rest[..consumed]);
                out.push_str(stand_in);
                rest = &value[end..];
                continue 'scan;
            }
            let width = rest.chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..width]);
            rest = &rest[width..];
        }
        out
    }

    /// Scrubs an exchange into a fixture, and refuses it if anything in it
    /// still holds the key.
    pub fn fixture(
        &mut self,
        request_body: &[u8],
        status: u16,
        headers: &Headers,
        response_body: &[u8],
        note: &str,
    ) -> Result<Fixture, FixtureError> {
        let fixture = Fixture {
            request: self.body(request_body),
            status,
            headers: self.headers(headers),
            response: self.body(response_body),
            note: note.to_owned(),
        };
        self.check("the request body", &fixture.request)?;
        self.check("the response body", &fixture.response)?;
        self.check("the note", fixture.note.as_bytes())?;
        for (name, value) in fixture.headers.iter() {
            self.check("a response header", name.as_bytes())?;
            self.check("a response header", value.as_bytes())?;
        }
        Ok(fixture)
    }
}

fn is_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

/// JSON text with line breaks and indentation added, and nothing else
/// changed: the order of keys, the escapes and the numbers stay as they
/// were written. For snapshots of request bodies, where the order of keys
/// is part of what is being checked.
#[must_use]
pub fn pretty(json: &str) -> String {
    let mut out = String::with_capacity(json.len() * 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let indent = |out: &mut String, depth: usize| {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    };
    let mut characters = json.chars().peekable();
    while let Some(character) = characters.next() {
        if in_string {
            out.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => {
                in_string = true;
                out.push(character);
            }
            '{' | '[' => {
                out.push(character);
                // An empty object or array stays on one line.
                let closing = if character == '{' { '}' } else { ']' };
                if characters.peek() == Some(&closing) {
                    out.push(closing);
                    characters.next();
                } else {
                    depth += 1;
                    indent(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                indent(&mut out, depth);
                out.push(character);
            }
            ',' => {
                out.push(character);
                indent(&mut out, depth);
            }
            ':' => out.push_str(": "),
            character if character.is_whitespace() => {}
            other => out.push(other),
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up key, in the shape of a real one.
    const KEY: &str = "sk-ant-api03-MADEUPmadeup0123456789abcdefMADEUPmadeup-notakey-AA";

    fn scrubber() -> Scrubber {
        Scrubber::new(Secret::new(KEY))
    }

    #[test]
    fn anything_that_holds_the_key_is_refused() {
        let scrubber = scrubber();
        let clean = b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert!(scrubber.check("the response body", clean).is_ok());

        let whole = format!("{{\"error\":\"invalid x-api-key {KEY}\"}}");
        let error = scrubber.check("the response body", whole.as_bytes()).unwrap_err();
        assert!(error.message.contains("refused"), "{error}");
        assert!(error.message.contains("the response body"));
        assert!(!error.message.contains(KEY), "the refusal does not repeat the key");
        assert!(!error.message.contains(&KEY[..KEY_WINDOW]));

        // Part of the key is enough: its start, its middle or its end.
        for part in [&KEY[..20], &KEY[25..40], &KEY[KEY.len() - KEY_WINDOW..]] {
            let echoed = format!("a provider that echoes \"{part}…\" in a message");
            assert!(
                scrubber.check("the output", echoed.as_bytes()).is_err(),
                "{part}"
            );
            assert!(scrubber.printable(&echoed).is_err());
        }
        // Fewer characters than the window are not the key.
        let short = format!("starts like a key: {}", &KEY[..KEY_WINDOW - 1]);
        assert!(scrubber.check("the output", short.as_bytes()).is_ok());
        assert_eq!(scrubber.printable("recorded 3 events").unwrap(), "recorded 3 events");
        // Bytes that are not text are searched too.
        let mut binary = vec![0xff, 0xfe, 0x00];
        binary.extend_from_slice(KEY.as_bytes());
        assert!(scrubber.check("the response body", &binary).is_err());
    }

    #[test]
    fn a_recording_that_holds_the_key_is_not_made_into_a_fixture() {
        let mut headers = Headers::new();
        headers.insert("content-type", "text/event-stream");
        let request = br#"{"model":"m","messages":[]}"#;
        let clean = b"event: ping\ndata: {\"type\":\"ping\"}\n\n";

        let mut scrubber = scrubber();
        assert!(scrubber.fixture(request, 200, &headers, clean, "A ping.").is_ok());

        // The key in the response body, in the request body, in a kept
        // header, and in the note: each is a refusal.
        let leaked = format!("data: {{\"message\":\"your key {KEY} is invalid\"}}\n\n");
        assert!(
            scrubber
                .fixture(request, 401, &headers, leaked.as_bytes(), "note")
                .is_err()
        );
        let sent = format!("{{\"metadata\":\"{KEY}\"}}");
        assert!(
            scrubber
                .fixture(sent.as_bytes(), 200, &headers, clean, "note")
                .is_err()
        );
        let mut echoing = headers.clone();
        echoing.insert("retry-after", KEY);
        assert!(scrubber.fixture(request, 200, &echoing, clean, "note").is_err());
        let note = format!("Recorded with {KEY}.");
        assert!(scrubber.fixture(request, 200, &headers, clean, &note).is_err());

        // A header that is not kept cannot leak it: it is not in the fixture.
        let mut dropped = headers.clone();
        dropped.insert("x-api-key", KEY);
        dropped.insert("authorization", format!("Bearer {KEY}"));
        let fixture = scrubber
            .fixture(request, 200, &dropped, clean, "note")
            .unwrap();
        assert_eq!(fixture.headers.len(), 1);
    }

    #[test]
    fn only_the_listed_response_headers_are_kept() {
        for kept in [
            "content-type",
            "Retry-After",
            "request-id",
            "anthropic-ratelimit-requests-remaining",
            "anthropic-ratelimit-input-tokens-reset",
            "anthropic-fast-output-tokens-limit",
            "anthropic-priority-input-tokens-limit",
        ] {
            assert!(keeps_header(kept), "{kept}");
        }
        for dropped in [
            "anthropic-organization-id",
            "anthropic-workspace-id",
            "x-api-key",
            "authorization",
            "set-cookie",
            "cf-ray",
            "server",
            "date",
            "via",
            "x-amzn-requestid",
            "anthropic-version",
        ] {
            assert!(!keeps_header(dropped), "{dropped}");
        }

        let mut scrubber = scrubber();
        let headers: Headers = [
            ("request-id", "req_011CSHoEeqs5C35K2UUqR7Fy"),
            ("anthropic-organization-id", "0f0f0f0f-aaaa-bbbb-cccc-121212121212"),
            ("anthropic-fast-input-tokens-remaining", "159000"),
            ("set-cookie", "session=abc"),
        ]
        .into_iter()
        .collect();
        let kept = scrubber.headers(&headers);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept.get("request-id"), Some("req_fixture0001"));
        assert_eq!(kept.get("anthropic-fast-input-tokens-remaining"), Some("159000"));
    }

    #[test]
    fn an_identifier_gets_the_same_stand_in_wherever_it_appears() {
        let mut scrubber = scrubber();
        let stream = scrubber.text(
            r#"{"id":"msg_01XFDUDYJgAACzvnptvVoYEL","content_block":{"type":"tool_use","id":"toolu_01T1x1fJ34qAmk2tNTrN7Up6","name":"read"}}"#,
        );
        assert_eq!(
            stream,
            r#"{"id":"msg_fixture0001","content_block":{"type":"tool_use","id":"toolu_fixture0002","name":"read"}}"#
        );
        // The next request names the same call, and so does its stand-in.
        let request = scrubber.text(
            r#"{"type":"tool_result","tool_use_id":"toolu_01T1x1fJ34qAmk2tNTrN7Up6","content":"x"},{"id":"toolu_01A09q90qw90lq917835lq9"}"#,
        );
        assert_eq!(
            request,
            r#"{"type":"tool_result","tool_use_id":"toolu_fixture0002","content":"x"},{"id":"toolu_fixture0003"}"#
        );

        // Names that only look like the start of an identifier stay.
        for kept in [
            r#"{"tool_use_id":"x","user_id":"u","file_path":"a/msg_1.txt"}"#,
            "input_tokens output_tokens stop_reason end_turn tool_use message_start",
            "a req_uest and a call_site, and somemsg_0123456789abcdef",
            "héllo → wörld",
        ] {
            assert_eq!(scrubber.text(kept), kept);
        }
    }

    #[test]
    fn an_opaque_payload_is_replaced_and_stays_the_same_everywhere() {
        let mut scrubber = scrubber();
        let payload = "EqQBCgIYAhIM1gbcDa9GJwZA2b3hGgxBdjrkzLoky3dl1pkiMOYds";
        let delta = format!(
            r#"data: {{"type":"content_block_delta","index":0,"delta":{{"type":"signature_delta","signature": "{payload}"}}}}"#
        );
        let scrubbed = scrubber.text(&delta);
        assert!(!scrubbed.contains(payload));
        assert!(scrubbed.contains(r#""signature": "fixture-signature-01""#), "{scrubbed}");

        // The same payload in the request that sends the block back.
        let block = format!(r#"{{"type":"thinking","thinking":"","signature":"{payload}"}}"#);
        assert_eq!(
            scrubber.text(&block),
            r#"{"type":"thinking","thinking":"","signature":"fixture-signature-01"}"#
        );
        let other = r#"{"type":"redacted_thinking","data":"AAAAbbbbCCCCddddEEEEffff0000"}"#;
        assert_eq!(
            scrubber.text(other),
            r#"{"type":"redacted_thinking","data":"fixture-data-02"}"#
        );
        // An empty or short value is not a payload: a block as it starts.
        let start = r#"{"type":"thinking","thinking":"","signature":""}"#;
        assert_eq!(scrubber.text(start), start);
        // An image's data is a payload too, and is not kept.
        let image = r#"{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"}"#;
        assert!(scrubber.text(image).contains("fixture-data-03"));
    }

    #[test]
    fn a_fixture_reads_back_as_it_was_written() {
        let directory = std::env::temp_dir().join(format!(
            "cortex-fixture-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let headers: Headers = [("content-type", "text/event-stream"), ("retry-after", "7")]
            .into_iter()
            .collect();
        let fixture = Fixture {
            request: br#"{"model":"m"}"#.to_vec(),
            status: 429,
            headers,
            response: b"event: ping\r\ndata: {}\r\n\r\n".to_vec(),
            note: "A made-up exchange.\n".to_owned(),
        };
        fixture.write(&directory).unwrap();
        let read = Fixture::load(&directory).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(read, fixture);
        assert!(Fixture::load(&directory).is_err(), "a fixture that is not there");
    }

    #[test]
    fn pretty_adds_only_white_space() {
        let json = r#"{"b":1,"a":{"z":[],"y":{},"x":[1,2.50,"t, {x}: [y] \"q\""]},"s":"a\\"}"#;
        let pretty = pretty(json);
        assert_eq!(
            pretty,
            "{\n  \"b\": 1,\n  \"a\": {\n    \"z\": [],\n    \"y\": {},\n    \"x\": [\n      1,\n      2.50,\n      \"t, {x}: [y] \\\"q\\\"\"\n    ]\n  },\n  \"s\": \"a\\\\\"\n}\n"
        );
        // The same value, and the keys in the order they were written.
        assert_eq!(
            serde_json::from_str::<Value>(&pretty).unwrap(),
            serde_json::from_str::<Value>(json).unwrap()
        );
        assert!(pretty.find("\"b\"").unwrap() < pretty.find("\"a\"").unwrap());
    }
}
