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
//! - **Identifiers that have no shape are found by where they are.** The
//!   servers of the `openai-chat` dialect each write ids their own way (a
//!   prefix and a hyphen, a bare UUID, a call id with underscores in it),
//!   so a recording of one is scrubbed with [`Scrubber::with_id_members`]:
//!   the value of every `id` and `tool_call_id` member is an identifier,
//!   and is replaced wherever it appears afterwards, in any exchange. A
//!   UUID is replaced wherever it is: one provider quotes its request id
//!   inside the message of a failure, where no member names it.
//! - **The size of the account's rate limits is made up.** The headers
//!   that are kept say how large each limit is and how much of it is left,
//!   which shows the usage tier of the account that recorded. Every
//!   `…-limit` is replaced by a round number chosen by what the limit
//!   counts, and every `…-remaining` by the same share of that number, so
//!   that "all of it", "none of it" and "one request used" still read as
//!   they did ([`made_up_limit`]).
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
    matches!(
        name.as_str(),
        "content-type" | "retry-after" | "request-id" | "x-request-id"
    ) || name.starts_with("anthropic-ratelimit-")
        || name.starts_with("anthropic-fast-")
        || generic_limit_family(&name).is_some()
}

/// The six rate-limit headers of the `openai-chat` dialect:
/// `x-ratelimit-{limit,remaining,reset}-{requests,tokens}`. Returns what
/// the header says (`limit`, `remaining` or `reset`) and what it counts.
/// Any other `x-ratelimit-…` header is not one of them, and is not kept.
fn generic_limit_family(lowercase_name: &str) -> Option<(&str, &str)> {
    let rest = lowercase_name.strip_prefix("x-ratelimit-")?;
    let (says, counts) = rest.split_once('-')?;
    (matches!(says, "limit" | "remaining" | "reset") && matches!(counts, "requests" | "tokens"))
        .then_some((says, counts))
}

/// The made-up size of a rate limit, by what the header's name says it
/// counts: requests, output tokens, or any other tokens. A fixture never
/// holds the size of a real account's limit.
#[must_use]
pub fn made_up_limit(header: &str) -> u64 {
    let header = header.to_ascii_lowercase();
    if header.contains("-requests-") || header.ends_with("-requests") {
        1_000
    } else if header.contains("-output-tokens-") {
        100_000
    } else {
        1_000_000
    }
}

/// Whether a kept header holds the size of a rate limit or what is left of
/// one.
fn is_limit_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if let Some((says, _)) = generic_limit_family(&name) {
        return says != "reset";
    }
    (name.starts_with("anthropic-ratelimit-") || name.starts_with("anthropic-fast-"))
        && (name.ends_with("-limit") || name.ends_with("-remaining"))
}

/// The prefixes of the identifiers a provider makes up, each followed by an
/// underscore and letters and digits.
const ID_PREFIXES: &[&str] = &[
    "msg",
    "req",
    "toolu",
    "srvtoolu",
    "org",
    "wrkspc",
    "user",
    "file",
    "container",
    "batch",
    "msgbatch",
    "resp",
    "call",
    "chatcmpl",
];

/// The members whose values are opaque payloads tied to an account.
const OPAQUE_MEMBERS: &[&str] = &["signature", "data", "encrypted_content", "encrypted_index"];

/// The shortest value of an id member that is taken for an identifier a
/// provider made up.
pub const SHORTEST_FOUND_ID: usize = 8;

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
    /// The members whose values are identifiers whatever they look like.
    id_members: &'static [&'static str],
    /// The identifiers found in those members so far, and their stand-ins.
    found_ids: BTreeMap<String, String>,
}

impl Scrubber {
    /// A scrubber that guards `key`.
    #[must_use]
    pub fn new(key: Secret) -> Self {
        Self {
            key,
            ids: BTreeMap::new(),
            opaque: BTreeMap::new(),
            id_members: &[],
            found_ids: BTreeMap::new(),
        }
    }

    /// Also takes the value of each of these members for an identifier,
    /// whatever it looks like: `"id": "…"`. Once such a value has been
    /// seen it is replaced wherever it appears, in this exchange and in
    /// every later one, so that a tool result still names its call.
    ///
    /// A value shorter than [`SHORTEST_FOUND_ID`] is left alone: it is a
    /// counter, not something a provider made up, and replacing every
    /// occurrence of a short text would change what the recording says.
    #[must_use]
    pub fn with_id_members(mut self, members: &'static [&'static str]) -> Self {
        self.id_members = members;
        self
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

    /// The response headers that are kept, with the request id replaced
    /// and the size of every rate limit made up.
    #[must_use]
    pub fn headers(&mut self, headers: &Headers) -> Headers {
        let mut kept = Headers::new();
        for (name, value) in headers.iter() {
            if !keeps_header(name) {
                continue;
            }
            let value = if is_limit_header(name) {
                limit_stand_in(name, value, headers)
            } else if name.to_ascii_lowercase().ends_with("request-id") {
                // The id of a request is an identifier whatever it looks
                // like: one with no shape this knows is replaced too.
                let scrubbed = self.text(value);
                if scrubbed == value && !value.contains("fixture") && !value.trim().is_empty() {
                    let next = self.ids.len() + 1;
                    self.ids
                        .entry(value.to_owned())
                        .or_insert_with(|| format!("req_fixture{next:04}"))
                        .clone()
                } else {
                    scrubbed
                }
            } else {
                self.text(value)
            };
            kept.insert(name, value);
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
        let text = self.replace_found_ids(&text);
        self.replace_ids(&text)
    }

    /// Notes the value of each member named in
    /// [`with_id_members`](Self::with_id_members), and replaces every
    /// identifier noted so far, wherever it appears.
    fn replace_found_ids(&mut self, text: &str) -> String {
        if self.id_members.is_empty() {
            return text.to_owned();
        }
        // Find them: a member's key, a colon, a string. Inside a string of
        // the document the quotes are written `\"`.
        for member in self.id_members {
            for quote in ["\\\"", "\""] {
                let key = format!("{quote}{member}{quote}");
                let mut rest = text;
                while let Some(at) = rest.find(&key) {
                    rest = &rest[at + key.len()..];
                    let Some(value) = rest
                        .trim_start()
                        .strip_prefix(':')
                        .and_then(|after| after.trim_start().strip_prefix(quote))
                    else {
                        continue;
                    };
                    let Some(end) = string_end(value, quote) else {
                        continue;
                    };
                    let id = &value[..end];
                    // Anything a provider could write as an identifier:
                    // no space, no quote, nothing that is not ASCII.
                    let plain = id
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\');
                    if id.len() >= SHORTEST_FOUND_ID
                        && plain
                        && !id.contains("fixture")
                        && !self.found_ids.contains_key(id)
                    {
                        let next = self.found_ids.len() + 1;
                        self.found_ids
                            .insert(id.to_owned(), format!("id_fixture{next:04}"));
                    }
                }
            }
        }
        // Replace them, the longest first, so that an identifier that
        // begins with another one is not cut in two.
        let mut known: Vec<(&String, &String)> = self.found_ids.iter().collect();
        known.sort_by_key(|(id, _)| std::cmp::Reverse(id.len()));
        let mut out = text.to_owned();
        for (id, stand_in) in known {
            if out.contains(id.as_str()) {
                out = out.replace(id.as_str(), stand_in);
            }
        }
        out
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
                        // underscore is a name, such as `user_id`. And a
                        // stand-in is not replaced again: a recording may
                        // quote an earlier fixture, whose stand-ins must
                        // stay the ones that fixture has.
                        (length >= 8 && !rest.starts_with("fixture"))
                            .then_some((*prefix, prefix.len() + 1 + length))
                    })
                })
                .flatten();
            let matched = matched.or_else(|| {
                // An identifier with no prefix and a shape of its own: a
                // UUID. One provider writes its request ids so, and quotes
                // them inside the message of a failure.
                (boundary && is_uuid(&bytes[at..])).then_some(("uuid", UUID_LENGTH))
            });
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
    ///
    /// Every value that is not empty is replaced, whatever it looks like:
    /// what is not recognised as a payload is taken for one. The member is
    /// found as a key of JSON, and also as a key of JSON that is itself
    /// inside a JSON string (`\"signature\": \"…\"`), which is how a
    /// block appears inside a tool result or an event that quotes one.
    fn replace_opaque(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        'scan: while !rest.is_empty() {
            for member in OPAQUE_MEMBERS {
                for quote in ["\\\"", "\""] {
                    let Some(after_key) = rest
                        .strip_prefix(quote)
                        .and_then(|rest| rest.strip_prefix(member))
                        .and_then(|rest| rest.strip_prefix(quote))
                    else {
                        continue;
                    };
                    let Some(after_colon) = after_key.trim_start().strip_prefix(':') else {
                        continue;
                    };
                    let Some(value) = after_colon.trim_start().strip_prefix(quote) else {
                        continue;
                    };
                    let Some(end) = string_end(value, quote) else {
                        continue;
                    };
                    let payload = &value[..end];
                    if payload.is_empty() {
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

/// What a fixture holds in place of the size of a rate limit, or of what is
/// left of one.
///
/// A limit becomes [`made_up_limit`]. What is left becomes the same share
/// of the made-up limit: nothing stays nothing, all of it stays all of it,
/// and anything between stays between, at least one. A value that cannot be
/// read as a number, or whose limit cannot, is taken for "all of it".
fn limit_stand_in(name: &str, value: &str, headers: &Headers) -> String {
    let made_up = made_up_limit(name);
    let number = |text: &str| text.trim().parse::<u64>().ok();
    let lower = name.to_ascii_lowercase();
    let limit_header = match generic_limit_family(&lower) {
        Some(("remaining", counts)) => format!("x-ratelimit-limit-{counts}"),
        Some(_) => return made_up.to_string(),
        None => match lower.strip_suffix("-remaining") {
            Some(family) => format!("{family}-limit"),
            None => return made_up.to_string(),
        },
    };
    let limit = headers
        .get(&limit_header)
        .and_then(number)
        .filter(|limit| *limit > 0);
    let left = match (number(value), limit) {
        (Some(0), _) => 0,
        (Some(left), Some(limit)) => {
            let share = u128::from(made_up) * u128::from(left) / u128::from(limit);
            u64::try_from(share).unwrap_or(made_up).clamp(1, made_up)
        }
        _ => made_up,
    };
    left.to_string()
}

/// Where a JSON string's content ends, given the text after its opening
/// quote: the offset of the quote that closes it. `quote` is `"` for a
/// string of the document itself, where a backslash escapes what follows
/// it, and `\"` for a string inside a string.
fn string_end(text: &str, quote: &str) -> Option<usize> {
    if quote != "\"" {
        return text.find(quote);
    }
    let mut escaped = false;
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'"' => return Some(at),
            _ => {}
        }
    }
    None
}

fn is_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

/// The length of a UUID as text: 32 hexadecimal digits in five groups.
const UUID_LENGTH: usize = 36;

/// Whether `bytes` begin with a UUID that ends there: `8-4-4-4-12`
/// hexadecimal digits, not followed by another letter or digit.
fn is_uuid(bytes: &[u8]) -> bool {
    let Some(candidate) = bytes.get(..UUID_LENGTH) else {
        return false;
    };
    let shaped = candidate.iter().enumerate().all(|(at, byte)| match at {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    });
    shaped && !bytes.get(UUID_LENGTH).copied().is_some_and(is_id_byte)
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
        let error = scrubber
            .check("the response body", whole.as_bytes())
            .unwrap_err();
        assert!(error.message.contains("refused"), "{error}");
        assert!(error.message.contains("the response body"));
        assert!(
            !error.message.contains(KEY),
            "the refusal does not repeat the key"
        );
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
        assert_eq!(
            scrubber.printable("recorded 3 events").unwrap(),
            "recorded 3 events"
        );
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
        assert!(
            scrubber
                .fixture(request, 200, &headers, clean, "A ping.")
                .is_ok()
        );

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
        assert!(
            scrubber
                .fixture(request, 200, &echoing, clean, "note")
                .is_err()
        );
        let note = format!("Recorded with {KEY}.");
        assert!(
            scrubber
                .fixture(request, 200, &headers, clean, &note)
                .is_err()
        );

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
        ] {
            assert!(keeps_header(kept), "{kept}");
        }
        for dropped in [
            // Capacity an account has reserved: nothing reads it, and it
            // says something about the account.
            "anthropic-priority-input-tokens-limit",
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
            (
                "anthropic-organization-id",
                "0f0f0f0f-aaaa-bbbb-cccc-121212121212",
            ),
            ("retry-after", "7"),
            ("set-cookie", "session=abc"),
        ]
        .into_iter()
        .collect();
        let kept = scrubber.headers(&headers);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept.get("request-id"), Some("req_fixture0001"));
        assert_eq!(kept.get("retry-after"), Some("7"));
    }

    #[test]
    fn the_size_of_a_rate_limit_is_made_up() {
        // Numbers no real account has, standing in for a real account's.
        let recorded: Headers = [
            ("anthropic-ratelimit-requests-limit", "7310"),
            ("anthropic-ratelimit-requests-remaining", "7309"),
            ("anthropic-ratelimit-requests-reset", "2026-11-13T05:39:18Z"),
            ("anthropic-ratelimit-tokens-limit", "3141000"),
            ("anthropic-ratelimit-tokens-remaining", "3141000"),
            ("anthropic-ratelimit-input-tokens-limit", "2718000"),
            ("anthropic-ratelimit-input-tokens-remaining", "1359000"),
            ("anthropic-ratelimit-output-tokens-limit", "577000"),
            ("anthropic-ratelimit-output-tokens-remaining", "0"),
            ("Anthropic-Fast-Input-Tokens-Limit", "912000"),
            ("Anthropic-Fast-Input-Tokens-Remaining", "3"),
            ("anthropic-fast-output-tokens-limit", "183000"),
            ("anthropic-fast-output-tokens-remaining", "183000"),
            ("retry-after", "7"),
        ]
        .into_iter()
        .collect();
        let mut scrubber = scrubber();
        let kept = scrubber.headers(&recorded);
        let expected = [
            // A limit is a round number chosen by what it counts.
            ("anthropic-ratelimit-requests-limit", "1000"),
            // One request used is still one request used.
            ("anthropic-ratelimit-requests-remaining", "999"),
            ("anthropic-ratelimit-tokens-limit", "1000000"),
            // All of it left is all of it left.
            ("anthropic-ratelimit-tokens-remaining", "1000000"),
            ("anthropic-ratelimit-input-tokens-limit", "1000000"),
            // Half left is half left.
            ("anthropic-ratelimit-input-tokens-remaining", "500000"),
            ("anthropic-ratelimit-output-tokens-limit", "100000"),
            // Nothing left is nothing left: that is what a refusal turns on.
            ("anthropic-ratelimit-output-tokens-remaining", "0"),
            ("anthropic-fast-input-tokens-limit", "1000000"),
            // Nearly nothing is not nothing.
            ("anthropic-fast-input-tokens-remaining", "3"),
            ("anthropic-fast-output-tokens-limit", "100000"),
            ("anthropic-fast-output-tokens-remaining", "100000"),
            // Times and waits are not sizes, and stay.
            ("anthropic-ratelimit-requests-reset", "2026-11-13T05:39:18Z"),
            ("retry-after", "7"),
        ];
        for (name, value) in expected {
            assert_eq!(kept.get(name), Some(value), "{name}");
        }
        // None of the recorded sizes is in what is kept.
        let written = format!("{kept:?}");
        for real in [
            "7310", "7309", "3141000", "2718000", "1359000", "577000", "912000", "183000",
        ] {
            assert!(!written.contains(real), "{real} in {written}");
        }
        // What the scrubber wrote, it leaves alone: a fixture can be checked
        // against it.
        let again = scrubber.headers(&kept);
        for (name, value) in kept.iter() {
            if name != "request-id" {
                assert_eq!(again.get(name), Some(value), "{name}");
            }
        }

        // What is left without a limit to measure it by, or a value that is
        // not a number, is taken for all of it.
        let odd: Headers = [
            ("anthropic-ratelimit-tokens-remaining", "88000"),
            ("anthropic-ratelimit-requests-limit", "many"),
            ("anthropic-ratelimit-requests-remaining", "some"),
            ("anthropic-fast-output-tokens-limit", "0"),
            ("anthropic-fast-output-tokens-remaining", "12"),
        ]
        .into_iter()
        .collect();
        let kept = scrubber.headers(&odd);
        assert_eq!(
            kept.get("anthropic-ratelimit-tokens-remaining"),
            Some("1000000")
        );
        assert_eq!(kept.get("anthropic-ratelimit-requests-limit"), Some("1000"));
        assert_eq!(
            kept.get("anthropic-ratelimit-requests-remaining"),
            Some("1000")
        );
        assert_eq!(
            kept.get("anthropic-fast-output-tokens-remaining"),
            Some("100000")
        );
        // And a whole recording goes through the same rule.
        let fixture = scrubber
            .fixture(b"{}", 200, &recorded, b"data: {}\n\n", "note")
            .unwrap();
        assert_eq!(
            fixture.headers.get("anthropic-ratelimit-requests-limit"),
            Some("1000")
        );
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
    fn a_uuid_is_an_identifier_wherever_it_is() {
        let mut scrubber = scrubber();
        // One provider quotes its request id inside the message of a
        // failure, where no member names it.
        let message = r#"{"error":{"message":"Authentication Fails (request_id: 1b2c3d4e-0000-4000-8000-0123456789a1)"}}"#;
        let scrubbed = scrubber.text(message);
        assert_eq!(
            scrubbed,
            r#"{"error":{"message":"Authentication Fails (request_id: uuid_fixture0001)"}}"#
        );
        // The same one again is the same stand-in, and another is another.
        let more = scrubber.text("1B2C3D4E-0000-4000-8000-0123456789AB 1b2c3d4e-0000-4000-8000-0123456789a1 00000000-0000-4000-8000-000000000001");
        assert_eq!(more, "uuid_fixture0002 uuid_fixture0001 uuid_fixture0003");
        // What only looks like the beginning of one is left alone, and so
        // is one that is the end of a longer word.
        for not_one in [
            "x1b2c3d4e-0000-4000-8000-0123456789a1",
            "1b2c3d4e-0000-4000-8000-0123456789a",
            "1b2c3d4e-0000-4000-8000-0123456789a1f",
            "1b2c3d4e_0000_4000_8000_0123456789ab",
            "g1b2c3d4-0000-4000-8000-0123456789ab",
        ] {
            assert_eq!(scrubber.text(not_one), not_one);
        }
    }

    #[test]
    fn an_identifier_in_a_member_may_hold_any_character_a_provider_writes() {
        let mut scrubber = scrubber().with_id_members(&["id"]);
        let scrubbed = scrubber.text(r#"{"id":"gen/17000+AbC=dEf","other":"gen/17000+AbC=dEf"}"#);
        assert_eq!(
            scrubbed,
            r#"{"id":"id_fixture0001","other":"id_fixture0001"}"#
        );
        // A sentence in an `id` member is not an identifier.
        let sentence = r#"{"id":"not an identifier"}"#;
        assert_eq!(scrubber.text(sentence), sentence);
    }

    #[test]
    fn a_stand_in_is_not_replaced_again() {
        // A recording that quotes an earlier fixture: the conversation a
        // second provider is handed holds the first one's stand-ins.
        let mut scrubber = scrubber();
        let quoted = r#"{"tool_call_id":"toolu_fixture0005","id":"msg_fixture0002"}"#;
        assert_eq!(scrubber.text(quoted), quoted);
        // A real identifier beside it is still replaced.
        let mixed = scrubber.text(r#"["toolu_fixture0005","toolu_01AbCdEfGhIjKlMn"]"#);
        assert_eq!(mixed, r#"["toolu_fixture0005","toolu_fixture0001"]"#);
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
        assert!(
            scrubbed.contains(r#""signature": "fixture-signature-01""#),
            "{scrubbed}"
        );

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
        // An empty value is a block as it starts, and stays.
        let start = r#"{"type":"thinking","thinking":"","signature":""}"#;
        assert_eq!(scrubber.text(start), start);
        // Anything else is replaced, whatever it looks like: a short value,
        // one with escapes in it, one that holds an escaped quote.
        for (value, written) in [
            ("abc", "abc"),
            ("EqQB\\/CgIY+AhIM", "EqQB\\/CgIY+AhIM"),
            ("a\\\"b\\\\", "a\\\"b\\\\"),
        ] {
            let block = format!(r#"{{"type":"thinking","signature":"{written}","after":1}}"#);
            let scrubbed = scrubber.text(&block);
            assert!(!scrubbed.contains(value), "{scrubbed}");
            assert!(scrubbed.starts_with(r#"{"type":"thinking","signature":"fixture-signature-"#));
            assert!(scrubbed.ends_with(r#"","after":1}"#), "{scrubbed}");
            assert!(
                serde_json::from_str::<Value>(&scrubbed).is_ok(),
                "{scrubbed}"
            );
        }
        // A block quoted inside a string, as a tool result that holds JSON
        // has it.
        let quoted = r#"{"type":"text","text":"{\"type\":\"thinking\",\"signature\": \"EqQBCgIYAhIM1gbcDa9GJwZA2b3h\"}"}"#;
        let scrubbed = scrubber.text(quoted);
        assert!(
            !scrubbed.contains("EqQBCgIYAhIM1gbcDa9GJwZA2b3h"),
            "{scrubbed}"
        );
        assert!(
            serde_json::from_str::<Value>(&scrubbed).is_ok(),
            "{scrubbed}"
        );
        // An image's data is a payload too, and is not kept.
        let image = r#"{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"}"#;
        let scrubbed = scrubber.text(image);
        assert!(scrubbed.contains(r#""data":"fixture-data-"#), "{scrubbed}");
        assert!(!scrubbed.contains("iVBORw0KGgo"));
    }

    #[test]
    fn the_other_dialects_limit_headers_are_kept_and_made_up_too() {
        let mut headers = Headers::new();
        headers.insert("x-request-id", "req_0123456789abcdef");
        headers.insert("x-ratelimit-limit-requests", "60");
        headers.insert("x-ratelimit-remaining-requests", "59");
        headers.insert("x-ratelimit-reset-requests", "1s");
        headers.insert("x-ratelimit-limit-tokens", "150000");
        headers.insert("x-ratelimit-remaining-tokens", "0");
        headers.insert("x-ratelimit-reset-tokens", "6m0s");
        // Not one of the six: a budget of failed requests, an account's
        // balance, a project's limit.
        headers.insert("x-ratelimit-remaining", "49");
        headers.insert("x-ratelimit-limit-project-tokens", "60000");
        headers.insert("x-venice-balance-usd", "12.50");
        headers.insert("x-ratelimit-limit-requests-day", "500");
        let kept = scrubber().headers(&headers);
        let names: Vec<&str> = kept.iter().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            [
                "x-request-id",
                "x-ratelimit-limit-requests",
                "x-ratelimit-remaining-requests",
                "x-ratelimit-reset-requests",
                "x-ratelimit-limit-tokens",
                "x-ratelimit-remaining-tokens",
                "x-ratelimit-reset-tokens",
            ]
        );
        assert_eq!(kept.get("x-request-id"), Some("req_fixture0001"));
        // A request id with no shape of its own is an identifier all the
        // same, and the same one is the same stand-in.
        let mut bare = Headers::new();
        bare.insert("x-request-id", "9f2c4e1ab07d");
        bare.insert("request-id", "9f2c4e1ab07d");
        let renamed = scrubber().headers(&bare);
        assert_eq!(renamed.get("x-request-id"), Some("req_fixture0001"));
        assert_eq!(renamed.get("request-id"), Some("req_fixture0001"));
        assert_eq!(kept.get("x-ratelimit-limit-requests"), Some("1000"));
        assert_eq!(kept.get("x-ratelimit-remaining-requests"), Some("983"));
        assert_eq!(kept.get("x-ratelimit-limit-tokens"), Some("1000000"));
        assert_eq!(kept.get("x-ratelimit-remaining-tokens"), Some("0"));
        // When a limit resets says nothing about its size.
        assert_eq!(kept.get("x-ratelimit-reset-tokens"), Some("6m0s"));
    }

    #[test]
    fn an_identifier_without_a_shape_is_found_by_its_member() {
        let mut scrubber = scrubber().with_id_members(&["id", "tool_call_id"]);
        let response = scrubber.text(concat!(
            r#"data: {"id":"0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9","choices":[{"index":0,"delta":"#,
            r#"{"tool_calls":[{"index":0,"id":"call_00_AbCdEfGhIjKl","function":{"name":"x"}}]}}]}"#,
            "\n\n",
            r#"data: {"id":"0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9","choices":[],"n":{"id":"7"}}"#,
        ));
        assert!(!response.contains("0a1b2c3d"), "{response}");
        assert!(!response.contains("AbCdEfGhIjKl"), "{response}");
        assert_eq!(response.matches("id_fixture0001").count(), 2);
        assert!(response.contains(r#""id":"id_fixture0002""#));
        // A counter is not an identifier anybody made up.
        assert!(response.contains(r#"{"id":"7"}"#));

        // A later request names the call: in its own member, and in a
        // sentence, and inside a string that quotes a block.
        let request = scrubber.text(concat!(
            r#"{"messages":[{"role":"tool","tool_call_id":"call_00_AbCdEfGhIjKl","content":"x"},"#,
            r#"{"role":"user","content":"the call `call_00_AbCdEfGhIjKl` and \"id\": \"gen-17000-QwErTyUiOp\""}]}"#,
        ));
        assert_eq!(request.matches("id_fixture0002").count(), 2, "{request}");
        assert!(request.contains("id_fixture0003"), "{request}");
        assert!(!request.contains("QwErTyUiOp"), "{request}");
        // A stand-in is not taken for an identifier to replace again.
        assert_eq!(scrubber.text(&request), request);

        // Without the members named, nothing of this happens: only an
        // identifier with a shape of its own is found.
        let mut plain = super::Scrubber::new(Secret::new(KEY));
        let untouched = r#"{"id":"gen-17000-QwErTyUiOp","tool_call_id":"call_00_AbCdEfGhIjKl"}"#;
        assert_eq!(plain.text(untouched), untouched);
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
        assert!(
            Fixture::load(&directory).is_err(),
            "a fixture that is not there"
        );
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
