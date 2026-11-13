//! The secret filter: masks what looks like a credential in text that is
//! about to be stored (`docs/security.md`, section 4).
//!
//! A [`SecretFilter`] knows two things:
//!
//! - **The harness's own credentials, by value.** Wherever the exact text of
//!   one appears, it is masked, whatever stands around it.
//! - **Well-known key formats.** A token that begins as the keys of a known
//!   service do and goes on as they do is masked: the list is
//!   [`FORMATS`], with a private-key block, a JSON web token, a bearer token
//!   and the password of a web address beside it.
//!
//! **It is best-effort, and says so.** A secret with no recognisable shape
//! (a database password, a key in a format of its own) passes through. So
//! does one that is encoded (base64, URL escapes), or split across two
//! pieces of text. The formats are kept narrow on purpose: what the filter
//! masks in a session log is what a resumed session then shows the model,
//! so a filter that masked ordinary source text would damage the
//! conversation.
//!
//! **It runs once per line of every text artifact,** so it is one pass over
//! the bytes with a table lookup per byte, and allocates only when it finds
//! something. The cost is measured by an ignored test of this module and
//! recorded in `docs/performance-budgets.md`.
//!
//! The filter is used through `cortex-session`'s `Redactor` hook, which
//! `cortex-core` fills with it, and by the log subscriber of this crate
//! (`cortex_base::log`).

use std::borrow::Cow;
use std::ops::Range;

use crate::secret::{REDACTED, Secret};

/// What takes a secret's place.
pub const MASK: &str = REDACTED;

/// The shortest credential that is masked by value. A shorter one would
/// mask ordinary text: a three-letter password is in every other word.
pub const MIN_VALUE_LEN: usize = 8;

/// A credential given by value can begin with this byte.
const VALUE: u8 = 1;
/// A key format, or one of the other shapes, can begin with this byte.
const SHAPE: u8 = 2;

/// Which characters a key's body is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Letters and digits.
    Alnum,
    /// Letters, digits, `_` and `-`.
    Token,
    /// Capital letters and digits.
    UpperDigit,
}

const ALNUM: u8 = 1;
const TOKEN: u8 = 2;
const UPPER_DIGIT: u8 = 4;
/// What a bearer token is made of: letters, digits and `-._~+/=`.
const BEARER: u8 = 8;
/// What the user part of a web address is made of: anything printable but
/// the characters that end it.
const USERINFO: u8 = 16;

/// The classes of each byte.
const CLASSES: [u8; 256] = classes();

const fn classes() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut byte = 0usize;
    while byte < 256 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the loop stops at 256, so the value fits a byte"
        )]
        let b = byte as u8;
        let mut class = 0u8;
        if b.is_ascii_alphanumeric() {
            class |= ALNUM | TOKEN | BEARER;
        }
        if b == b'_' || b == b'-' {
            class |= TOKEN | BEARER;
        }
        if b.is_ascii_uppercase() || b.is_ascii_digit() {
            class |= UPPER_DIGIT;
        }
        if matches!(b, b'.' | b'~' | b'+' | b'/' | b'=') {
            class |= BEARER;
        }
        if b.is_ascii_graphic() && !matches!(b, b'/' | b'@' | b'"' | b'\'' | b'<' | b'>' | b'\\') {
            class |= USERINFO;
        }
        table[byte] = class;
        byte += 1;
    }
    table
}

impl Class {
    const fn bit(self) -> u8 {
        match self {
            Self::Alnum => ALNUM,
            Self::Token => TOKEN,
            Self::UpperDigit => UPPER_DIGIT,
        }
    }
}

fn is(byte: u8, class: u8) -> bool {
    CLASSES[usize::from(byte)] & class != 0
}

/// One well-known key format: a fixed beginning, and a body of one class of
/// characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    /// Whose key it is, for a person reading the list.
    pub name: &'static str,
    /// What every such key begins with.
    pub prefix: &'static str,
    class: Class,
    /// The fewest characters after the prefix.
    min: usize,
    /// The most characters after the prefix, for a key of a fixed length.
    /// A longer run is not this key.
    max: Option<usize>,
    /// Whether the body must hold a digit. For a prefix short enough to
    /// begin an ordinary hyphenated name.
    digit: bool,
}

const fn format(
    name: &'static str,
    prefix: &'static str,
    class: Class,
    min: usize,
    max: Option<usize>,
    digit: bool,
) -> Format {
    Format {
        name,
        prefix,
        class,
        min,
        max,
        digit,
    }
}

/// The key formats the filter knows, each from its service's own
/// documentation of what its keys look like. A prefix that begins another
/// comes after it.
pub const FORMATS: &[Format] = &[
    format("Anthropic", "sk-ant-", Class::Token, 20, None, false),
    format("Stripe", "sk_live_", Class::Alnum, 16, None, false),
    format("Stripe", "sk_test_", Class::Alnum, 16, None, false),
    format("Stripe", "rk_live_", Class::Alnum, 16, None, false),
    format("Stripe", "rk_test_", Class::Alnum, 16, None, false),
    // OpenAI and the many services that copied its `sk-`. Short enough to
    // begin a class name in a stylesheet, so the body must hold a digit.
    format("OpenAI and others", "sk-", Class::Token, 20, None, true),
    format("GitHub", "ghp_", Class::Alnum, 36, None, false),
    format("GitHub", "gho_", Class::Alnum, 36, None, false),
    format("GitHub", "ghu_", Class::Alnum, 36, None, false),
    format("GitHub", "ghs_", Class::Alnum, 36, None, false),
    format("GitHub", "ghr_", Class::Alnum, 36, None, false),
    format("GitHub", "github_pat_", Class::Token, 22, None, false),
    format("GitLab", "glpat-", Class::Token, 20, None, false),
    format("AWS", "AKIA", Class::UpperDigit, 16, Some(16), false),
    format("AWS", "ASIA", Class::UpperDigit, 16, Some(16), false),
    format("Google", "AIza", Class::Token, 35, Some(35), false),
    format("Slack", "xoxb-", Class::Token, 10, None, true),
    format("Slack", "xoxp-", Class::Token, 10, None, true),
    format("Slack", "xoxa-", Class::Token, 10, None, true),
    format("Slack", "xoxr-", Class::Token, 10, None, true),
    format("Slack", "xoxs-", Class::Token, 10, None, true),
    format("Slack", "xoxe-", Class::Token, 10, None, true),
    format("npm", "npm_", Class::Alnum, 36, None, false),
    format("Hugging Face", "hf_", Class::Alnum, 30, None, false),
    format("PyPI", "pypi-", Class::Token, 50, None, false),
];

/// For every pair of bytes, whether a format's prefix begins with it: one
/// bit each.
const FORMAT_PAIRS: [u64; 1024] = format_pairs();

const fn format_pairs() -> [u64; 1024] {
    let mut table = [0u64; 1024];
    let mut n = 0;
    while n < FORMATS.len() {
        let prefix = FORMATS[n].prefix.as_bytes();
        let code = (prefix[0] as usize) << 8 | prefix[1] as usize;
        table[code >> 6] |= 1 << (code & 63);
        n += 1;
    }
    table
}

/// The beginning of a private key in PEM form, up to the kind of key.
const PEM_BEGIN: &str = "-----BEGIN ";
/// What ends the first line of a private key in PEM form.
const PEM_PRIVATE: &str = "PRIVATE KEY-----";
/// The beginning of the line that ends a key in PEM form.
const PEM_END: &str = "-----END ";
/// The longest name between `BEGIN` and `PRIVATE KEY`: `ENCRYPTED `,
/// `OPENSSH `, `RSA `, `EC `, `DSA `, `PGP `.
const PEM_KIND_MAX: usize = 16;
/// What begins each of the first two parts of a JSON web token: `{"` in
/// base64.
const JWT_BEGIN: &str = "eyJ";
/// The fewest characters of one part of a JSON web token.
const JWT_PART_MIN: usize = 10;
/// What stands before a bearer token.
const BEARER_WORD: &str = "Bearer ";
/// The fewest characters of a bearer token.
const BEARER_MIN: usize = 20;
/// What separates a web address's scheme from the rest.
const SCHEME_END: &str = "://";

/// Masks credentials in text: the harness's own by value, and anything in a
/// well-known key format.
///
/// It keeps the values it was given, so it prints none of them.
#[derive(Clone)]
pub struct SecretFilter {
    /// The credentials masked by value, longest first, so that a key that
    /// begins as another one does is masked whole.
    values: Vec<Box<[u8]>>,
    /// For each byte, what the filter masks that can begin with it:
    /// [`VALUE`], [`SHAPE`], both or neither.
    starts: [u8; 256],
}

impl Default for SecretFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SecretFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretFilter")
            .field("values", &self.values.len())
            .field("formats", &FORMATS.len())
            .finish()
    }
}

impl SecretFilter {
    /// A filter that knows the key formats and no credential by value.
    #[must_use]
    pub fn new() -> Self {
        let mut starts = [0u8; 256];
        for format in FORMATS {
            starts[usize::from(format.prefix.as_bytes()[0])] = SHAPE;
        }
        for other in [PEM_BEGIN, JWT_BEGIN, BEARER_WORD, SCHEME_END] {
            starts[usize::from(other.as_bytes()[0])] = SHAPE;
        }
        Self {
            values: Vec::new(),
            starts,
        }
    }

    /// Also masks `secret` wherever its exact text appears.
    ///
    /// A credential shorter than [`MIN_VALUE_LEN`] bytes is not added: it
    /// would mask ordinary text. Whether it was added is the answer of
    /// [`knows`](Self::knows).
    #[must_use]
    pub fn with_value(mut self, secret: &Secret) -> Self {
        self.add_value(secret);
        self
    }

    /// As [`with_value`](Self::with_value), on a filter that exists.
    pub fn add_value(&mut self, secret: &Secret) {
        let value = secret.expose().trim().as_bytes();
        if value.len() < MIN_VALUE_LEN || self.values.iter().any(|known| &**known == value) {
            return;
        }
        self.starts[usize::from(value[0])] |= VALUE;
        self.values.push(value.into());
        self.values
            .sort_by_key(|value| std::cmp::Reverse(value.len()));
    }

    /// How many credentials are masked by value.
    #[must_use]
    pub fn values(&self) -> usize {
        self.values.len()
    }

    /// Whether `secret` is one of the credentials masked by value.
    #[must_use]
    pub fn knows(&self, secret: &Secret) -> bool {
        let value = secret.expose().trim().as_bytes();
        self.values.iter().any(|known| &**known == value)
    }

    /// The text with every credential in it replaced by [`MASK`]. Text with
    /// nothing to mask comes back borrowed, at the cost of one pass.
    #[must_use]
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let bytes = text.as_bytes();
        let Some(first) = self.next(bytes, 0) else {
            return Cow::Borrowed(text);
        };
        let mut masked = String::with_capacity(text.len());
        let mut copied = 0;
        let mut found = Some(first);
        while let Some(range) = found {
            // A match begins and ends between characters: every byte the
            // filter matches on is ASCII, or is a whole credential the
            // caller gave as text.
            masked.push_str(&text[copied..range.start]);
            masked.push_str(MASK);
            copied = range.end;
            found = self.next(bytes, copied);
        }
        masked.push_str(&text[copied..]);
        Cow::Owned(masked)
    }

    /// Whether [`redact`](Self::redact) would change `text`. It stops at
    /// the first credential.
    #[must_use]
    pub fn finds(&self, text: &str) -> bool {
        self.next(text.as_bytes(), 0).is_some()
    }

    /// The first credential at or after `from`.
    fn next(&self, bytes: &[u8], from: usize) -> Option<Range<usize>> {
        let mut at = from;
        while at < bytes.len() {
            let starts = self.starts[usize::from(bytes[at])];
            if starts != 0
                && let Some(found) = self.matches_at(bytes, at, starts)
            {
                return Some(found);
            }
            at += 1;
        }
        None
    }

    /// The credential that `at` is the beginning of, if it is one. What is
    /// masked can begin after `at`: the word before a bearer token stays,
    /// and so does the colon before a password.
    fn matches_at(&self, bytes: &[u8], at: usize, starts: u8) -> Option<Range<usize>> {
        let rest = &bytes[at..];
        if starts & VALUE != 0 {
            for value in &self.values {
                if rest.starts_with(value) {
                    return Some(at..at + value.len());
                }
            }
        }
        if starts & SHAPE == 0 {
            return None;
        }
        match rest[0] {
            b':' => return userinfo(bytes, at),
            b'-' => return pem(rest).map(|len| at..at + len),
            _ => {}
        }
        // A key is a word of its own: `task-…` does not hold an `sk-` key.
        if at > 0 && is(bytes[at - 1], TOKEN) {
            return None;
        }
        match rest[0] {
            b'e' => jwt(rest).map(|len| at..at + len),
            b'B' => bearer(rest).map(|len| at + BEARER_WORD.len()..at + len),
            first => {
                // Most words that begin as a key does are not one, and the
                // second byte says so before any format is tried.
                let code = usize::from(first) << 8 | usize::from(*rest.get(1)?);
                if FORMAT_PAIRS[code >> 6] & (1 << (code & 63)) == 0 {
                    return None;
                }
                FORMATS
                    .iter()
                    .find_map(|format| format.matches(rest))
                    .map(|len| at..at + len)
            }
        }
    }
}

impl Format {
    /// How long the key at the start of `rest` is, if `rest` starts with one.
    fn matches(&self, rest: &[u8]) -> Option<usize> {
        let body = rest.strip_prefix(self.prefix.as_bytes())?;
        let class = self.class.bit();
        let run = body.iter().take_while(|byte| is(**byte, class)).count();
        if run < self.min || self.max.is_some_and(|max| run > max) {
            return None;
        }
        if self.digit && !body[..run].iter().any(u8::is_ascii_digit) {
            return None;
        }
        Some(self.prefix.len() + run)
    }
}

/// A JSON web token at the start of `rest`: three runs of base64 with dots
/// between them, the first two beginning as a JSON object does.
fn jwt(rest: &[u8]) -> Option<usize> {
    let part = |from: usize, object: bool| -> Option<usize> {
        let part = rest.get(from..)?;
        if object && !part.starts_with(JWT_BEGIN.as_bytes()) {
            return None;
        }
        let run = part.iter().take_while(|byte| is(**byte, TOKEN)).count();
        (run >= JWT_PART_MIN).then_some(from + run)
    };
    let header = part(0, true)?;
    if rest.get(header) != Some(&b'.') {
        return None;
    }
    let claims = part(header + 1, true)?;
    if rest.get(claims) != Some(&b'.') {
        return None;
    }
    part(claims + 1, false)
}

/// A bearer token after `Bearer ` at the start of `rest`. The answer is
/// where the token ends, counted from the word, which stays.
fn bearer(rest: &[u8]) -> Option<usize> {
    let token = rest.strip_prefix(BEARER_WORD.as_bytes())?;
    let run = token.iter().take_while(|byte| is(**byte, BEARER)).count();
    if run < BEARER_MIN || !token[..run].iter().any(u8::is_ascii_digit) {
        return None;
    }
    Some(BEARER_WORD.len() + run)
}

/// A private key in PEM form at the start of `rest`: from its first line to
/// the line that ends it, or to the end of the text when that line is not
/// there (a key cut short, or a text that is one line of a longer one).
fn pem(rest: &[u8]) -> Option<usize> {
    let after = rest.strip_prefix(PEM_BEGIN.as_bytes())?;
    let window = &after[..after.len().min(PEM_KIND_MAX + PEM_PRIVATE.len())];
    let kind = find(window, PEM_PRIVATE.as_bytes())?;
    if !after[..kind]
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || *byte == b' ')
    {
        return None;
    }
    let body = PEM_BEGIN.len() + kind + PEM_PRIVATE.len();
    let Some(end) = find(&rest[body..], PEM_END.as_bytes()) else {
        return Some(rest.len());
    };
    let last = body + end + PEM_END.len();
    // To the end of the closing line's own dashes.
    let closing = find(&rest[last..], b"-----").map_or(rest.len(), |at| last + at + 5);
    Some(closing)
}

/// The password of a web address: `scheme://user:password@host`. `at` is a
/// colon. This is called for the `:` that begins `://`, finds nothing
/// there, and is called again for the `:` between the user and the
/// password, which is the one it answers for. Only the password is masked.
fn userinfo(bytes: &[u8], at: usize) -> Option<Range<usize>> {
    // Is this the colon between a user and a password? Look back for the
    // `://` that the user part follows.
    let before = &bytes[..at];
    let user = before
        .iter()
        .rev()
        .take_while(|byte| is(**byte, USERINFO) && **byte != b':')
        .count();
    if user == 0 || !before[..at - user].ends_with(SCHEME_END.as_bytes()) {
        return None;
    }
    let after = &bytes[at + 1..];
    let password = after.iter().take_while(|byte| is(**byte, USERINFO)).count();
    if password == 0 || after.get(password) != Some(&b'@') {
        return None;
    }
    Some(at + 1..at + 1 + password)
}

/// Where `needle` first is in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::{self, ensure};
    use crate::rng::Rng;

    const OWN: &str = "cortex-test-key-7f3a9c";

    fn filter() -> SecretFilter {
        SecretFilter::new().with_value(&Secret::new(OWN))
    }

    #[test]
    fn the_harnesss_own_key_is_masked_wherever_it_stands() {
        let filter = filter();
        for text in [
            OWN.to_owned(),
            format!("KEY={OWN}"),
            format!("x{OWN}y"),
            format!("{{\"key\":\"{OWN}\"}}"),
            format!("first {OWN} and again {OWN}."),
        ] {
            let masked = filter.redact(&text);
            assert!(!masked.contains(OWN), "{masked}");
            assert!(masked.contains(MASK), "{masked}");
            assert!(filter.finds(&text));
        }
        assert_eq!(filter.redact(&format!("a {OWN} b")), "a [redacted] b");
    }

    #[test]
    fn a_short_value_is_not_added() {
        let filter = SecretFilter::new().with_value(&Secret::new("abc"));
        assert_eq!(filter.values(), 0);
        assert!(!filter.knows(&Secret::new("abc")));
        assert!(matches!(filter.redact("abc abc"), Cow::Borrowed(_)));
        // The same key twice is one value.
        let filter = self::filter().with_value(&Secret::new(OWN));
        assert_eq!(filter.values(), 1);
        assert!(filter.knows(&Secret::new(OWN)));
    }

    #[test]
    fn the_longer_of_two_keys_that_begin_alike_is_masked_whole() {
        let filter = SecretFilter::new()
            .with_value(&Secret::new("abcdefgh"))
            .with_value(&Secret::new("abcdefghijkl"));
        assert_eq!(filter.redact("abcdefghijkl"), MASK);
        assert_eq!(filter.redact("abcdefghXY"), "[redacted]XY");
    }

    /// One key of each format, made up: none of them is anybody's.
    fn samples() -> Vec<(&'static str, String)> {
        let alnum = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8s9T0";
        vec![
            ("Anthropic", format!("sk-ant-api03-{alnum}")),
            ("Stripe", format!("sk_live_{}", &alnum[..24])),
            ("OpenAI", format!("sk-proj-{alnum}")),
            ("GitHub", format!("ghp_{}", &alnum[..36])),
            ("GitHub", format!("github_pat_{}_{alnum}", &alnum[..22])),
            ("GitLab", format!("glpat-{}", &alnum[..20])),
            ("AWS", "AKIAIOSFODNN7EXAMPLE".to_owned()),
            ("Google", format!("AIza{}", &alnum[..35])),
            ("Slack", "xoxb-123456789012-abcdefABCDEF".to_owned()),
            ("npm", format!("npm_{}", &alnum[..36])),
            ("Hugging Face", format!("hf_{}", &alnum[..34])),
            ("PyPI", format!("pypi-{alnum}{alnum}")),
            (
                "JWT",
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk"
                    .to_owned(),
            ),
        ]
    }

    #[test]
    fn every_known_format_is_masked() {
        let filter = SecretFilter::new();
        for (name, key) in samples() {
            for text in [
                key.clone(),
                format!("TOKEN={key}"),
                format!("token: \"{key}\","),
                format!("export X='{key}'\n"),
            ] {
                let masked = filter.redact(&text);
                assert!(!masked.contains(&key), "{name}: {masked}");
                assert!(filter.finds(&text), "{name}");
            }
            assert_eq!(
                filter.redact(&format!("a {key} b")),
                "a [redacted] b",
                "{name}"
            );
        }
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        let filter = filter();
        for text in [
            "",
            "fn main() { println!(\"hello\"); }",
            // A prefix inside a word is not a key.
            "the task-runner-configuration-0123456789 is fine",
            "disk-usage-report-2026-11-13-final-version",
            // A stylesheet's class names begin `sk-` and hold no digit.
            ".sk-folding-cube-container-wrapper-element { }",
            // Too short to be a key.
            "sk-12345 ghp_short AKIA123 hf_abc npm_x",
            // A fixed-length key that goes on is something else.
            "AKIAIOSFODNN7EXAMPLEANDMORE",
            // A public key and a certificate are not secrets.
            "-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----",
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
            "https://example.com/path:with@colon",
            "https://user@example.com/",
            "see http://localhost:8080/ and ssh://git@host:22/repo",
            "Bearer of bad news, the long-winded-explanation follows",
            "eyJ is how base64 of a JSON object begins",
            "a line of dashes ----- and more -----",
        ] {
            assert!(
                matches!(filter.redact(text), Cow::Borrowed(_)),
                "masked: {text}"
            );
            assert!(!filter.finds(text), "{text}");
        }
    }

    #[test]
    fn a_private_key_is_masked_from_its_first_line_to_its_last() {
        let filter = SecretFilter::new();
        for kind in ["", "RSA ", "EC ", "OPENSSH ", "ENCRYPTED "] {
            let key = format!(
                "-----BEGIN {kind}PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nBKcwggSjAgEAAoIBAQC7\n-----END {kind}PRIVATE KEY-----"
            );
            assert_eq!(
                filter.redact(&format!("before\n{key}\nafter")),
                "before\n[redacted]\nafter",
                "{kind}"
            );
        }
        // Cut short, or one line of a longer text: to the end.
        assert_eq!(
            filter.redact("x -----BEGIN PRIVATE KEY-----\nMIIEvQ"),
            "x [redacted]"
        );
        assert_eq!(filter.redact("-----BEGIN RSA PRIVATE KEY-----"), MASK);
    }

    #[test]
    fn a_bearer_token_and_a_password_in_an_address_are_masked() {
        let filter = SecretFilter::new();
        assert_eq!(
            filter.redact("Authorization: Bearer abcDEF123456ghiJKL7890.-_~+/="),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            filter.redact("proxy = http://alice:hunter2@proxy.example:3128/"),
            "proxy = http://alice:[redacted]@proxy.example:3128/"
        );
        assert_eq!(
            filter.redact("git clone https://x-access-token:s3cr3t@github.com/a/b.git"),
            "git clone https://x-access-token:[redacted]@github.com/a/b.git"
        );
    }

    #[test]
    fn text_that_is_not_ascii_survives_around_a_key() {
        let filter = filter();
        let text = format!("ключ «{OWN}» найден — 鍵 {OWN}🔑");
        assert_eq!(
            filter.redact(&text),
            "ключ «[redacted]» найден — 鍵 [redacted]🔑"
        );
        // A credential that is not ASCII itself is masked whole too.
        let filter = SecretFilter::new().with_value(&Secret::new("пароль-пароль"));
        assert_eq!(filter.redact("это пароль-пароль!"), "это [redacted]!");
    }

    /// Text made of the pieces that begin and end keys, so that a generated
    /// input often nearly holds one.
    fn noisy(rng: &mut crate::rng::SeededRng) -> String {
        const PIECES: &[&str] = &[
            "sk-",
            "sk-ant-",
            "ghp_",
            "AKIA",
            "eyJ",
            ".",
            "-----BEGIN ",
            "PRIVATE KEY-----",
            "-----END ",
            "Bearer ",
            "://",
            ":",
            "@",
            " ",
            "\n",
            "a1B2c3D4e5",
            "0123456789",
            "ABCDEFGHIJKLMNOP",
            "é",
            "鍵",
            "x",
            "-",
            "_",
            OWN,
        ];
        let mut text = String::new();
        for _ in 0..rng.below(24) {
            let piece = usize::try_from(rng.below(PIECES.len() as u64)).unwrap_or(0);
            text.push_str(PIECES[piece]);
        }
        text
    }

    #[test]
    fn redaction_never_leaves_the_harnesss_key_and_keeps_the_rest() {
        let filter = filter();
        cases::check("redaction", 2000, noisy, |text| {
            let masked = filter.redact(text);
            ensure!(!masked.contains(OWN), "the key is still there: {masked}");
            ensure!(
                filter.finds(text) == matches!(masked, Cow::Owned(_)),
                "`finds` and `redact` disagree"
            );
            // What was not a key is still there, in order.
            let mut rest = text.as_str();
            for kept in masked.split(MASK) {
                let Some(at) = rest.find(kept) else {
                    return Err(format!("`{kept}` is not from the text, in order").into());
                };
                rest = &rest[at + kept.len()..];
            }
            Ok(())
        });
    }

    /// How long the filter takes on a large artifact. Run by hand:
    /// `cargo test -p cortex-base --release -- --ignored measure_redaction`.
    #[test]
    #[ignore = "a measurement, run by hand"]
    #[expect(clippy::print_stderr, reason = "a measurement prints its result")]
    fn measure_redaction_of_a_large_artifact() {
        let filter = SecretFilter::new()
            .with_value(&Secret::new(OWN))
            .with_value(&Secret::new("another-provider-key-0123456789abcdef"));
        // A build log: lines of ordinary output, 64 MiB of them, with a key
        // in one line of every thousand.
        let line = "   Compiling cortex-tools v0.0.0 (crates/cortex-tools) warning: unused variable `x` --> src/lib.rs:12:9\n";
        let with_key = format!("export ANTHROPIC_API_KEY={OWN} # oops\n");
        let lines = (64 << 20) / line.len();
        let started = std::time::Instant::now();
        let mut bytes = 0usize;
        let mut masked = 0usize;
        for n in 0..lines {
            let text = if n % 1000 == 0 {
                with_key.as_str()
            } else {
                line
            };
            bytes += text.len();
            if let Cow::Owned(_) = filter.redact(text) {
                masked += 1;
            }
        }
        let elapsed = started.elapsed();
        let mib = bytes as f64 / f64::from(1 << 20);
        eprintln!(
            "redacted {mib:.0} MiB in {lines} lines ({masked} masked) in {elapsed:?}: {:.0} MiB/s, {:.0} ns a line",
            mib / elapsed.as_secs_f64(),
            elapsed.as_nanos() as f64 / lines as f64,
        );
    }
}
