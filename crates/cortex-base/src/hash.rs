//! Content hashes: one way to name a piece of content by what it is.
//!
//! A [`ContentHash`] is what a file snapshot records
//! (`docs/architecture/05-tools.md`, "File snapshots"), what a stored file
//! version is filed under, and what a key derived from a path is made of.
//! Two contents with the same hash are taken to be the same content, so the
//! hash has to be one that nobody can make collide: it is SHA-256, from the
//! `sha2` crate, which uses the processor's SHA instructions where it has
//! them.
//!
//! The algorithm stays behind the type. Nothing outside this module names
//! it, so that changing it later is a change to this file and to whatever
//! was stored under the old hashes.

use std::fmt;
use std::io;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// How many bytes a hash has.
const LEN: usize = 32;

/// The hash of some content. Equal contents have equal hashes, and for every
/// practical purpose different contents have different ones.
///
/// It is written as 64 lowercase hexadecimal digits, by `Display`, in JSON
/// and in file names. The form is safe as a file name on every OS.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash([u8; LEN]);

impl ContentHash {
    /// The hash of `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = ContentHasher::new();
        hasher.update(bytes);
        hasher.finish()
    }

    /// The hash as 64 lowercase hexadecimal digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.to_string()
    }

    /// Reads the form [`to_hex`](Self::to_hex) writes. `None` for anything
    /// else: another length, an upper-case digit, a character that is not a
    /// digit. Exactly one text names each hash, so that a hash used as a
    /// file name names one file.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let digits = text.as_bytes();
        if digits.len() != LEN * 2 {
            return None;
        }
        let mut bytes = [0u8; LEN];
        let (pairs, _) = digits.as_chunks::<2>();
        for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (hex_value(*high)? << 4) | hex_value(*low)?;
        }
        Some(Self(bytes))
    }

    /// The hash's 32 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; LEN] {
        &self.0
    }
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = [0u8; LEN * 2];
        let (pairs, _) = text.as_chunks_mut::<2>();
        for ([high, low], byte) in pairs.iter_mut().zip(self.0) {
            *high = DIGITS[usize::from(byte >> 4)];
            *low = DIGITS[usize::from(byte & 0x0f)];
        }
        // Every byte written is an ASCII digit.
        f.write_str(std::str::from_utf8(&text).map_err(|_| fmt::Error)?)
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({self})")
    }
}

impl std::str::FromStr for ContentHash {
    type Err = InvalidHash;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_hex(text).ok_or(InvalidHash)
    }
}

/// A text that is not a content hash: not 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a content hash is 64 lowercase hexadecimal digits")]
pub struct InvalidHash;

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        Self::from_hex(&text).ok_or_else(|| serde::de::Error::custom(InvalidHash))
    }
}

/// Hashes content that arrives in pieces: a file read as a stream.
///
/// It is also a [`std::io::Write`], so `std::io::copy` can feed it.
#[derive(Clone, Default)]
pub struct ContentHasher(Sha256);

impl ContentHasher {
    /// A hasher that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the next piece of the content.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The hash of everything added, in the order it was added.
    #[must_use]
    pub fn finish(self) -> ContentHash {
        ContentHash(self.0.finalize().into())
    }
}

impl fmt::Debug for ContentHasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentHasher")
    }
}

impl io::Write for ContentHasher {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::{self, ensure};
    use crate::rng::Rng;

    /// The SHA-256 examples of FIPS 180-4 and of NIST's example values: the
    /// empty message, one block, two blocks, and a message of 112 bytes
    /// whose padding needs a block of its own.
    const VECTORS: &[(&str, &str)] = &[
        (
            "",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
             ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        ),
    ];

    #[test]
    fn the_standards_examples_hash_to_the_standards_values() {
        for (message, expected) in VECTORS {
            assert_eq!(ContentHash::of(message.as_bytes()).to_hex(), *expected);
        }
    }

    #[test]
    fn a_million_letters_hash_to_the_standards_value() {
        // The long example of FIPS 180-4, fed in uneven pieces.
        let mut hasher = ContentHasher::new();
        let piece = [b'a'; 997];
        let mut left = 1_000_000;
        while left > 0 {
            let take = left.min(piece.len());
            hasher.update(&piece[..take]);
            left -= take;
        }
        assert_eq!(
            hasher.finish().to_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn content_in_pieces_hashes_like_content_in_one() {
        cases::check(
            "hash_pieces",
            200,
            |rng| {
                let mut content = vec![0u8; cases::small(rng, 300)];
                rng.fill(&mut content);
                let cut = rng.below(content.len() as u64 + 1) as usize;
                (content, cut)
            },
            |(content, cut)| {
                let mut hasher = ContentHasher::new();
                hasher.update(&content[..*cut]);
                io::Write::write_all(&mut hasher, &content[*cut..])?;
                ensure!(
                    hasher.finish() == ContentHash::of(content),
                    "the pieces hash differently"
                );
                Ok(())
            },
        );
    }

    #[test]
    fn a_hash_reads_back_from_its_text_and_from_json() {
        let hash = ContentHash::of(b"content");
        let text = hash.to_hex();
        assert_eq!(text.len(), 64);
        assert_eq!(ContentHash::from_hex(&text), Some(hash));
        assert_eq!(text.parse::<ContentHash>(), Ok(hash));
        assert_eq!(format!("{hash:?}"), format!("ContentHash({text})"));

        let json = serde_json::to_string(&hash).expect("a hash is a string");
        assert_eq!(json, format!("\"{text}\""));
        let back: ContentHash = serde_json::from_str(&json).expect("it reads back");
        assert_eq!(back, hash);
    }

    #[test]
    fn only_the_one_spelling_of_a_hash_is_read() {
        let text = ContentHash::of(b"content").to_hex();
        assert_eq!(ContentHash::from_hex(&text.to_uppercase()), None);
        assert_eq!(ContentHash::from_hex(&text[1..]), None);
        assert_eq!(ContentHash::from_hex(&format!("{text}0")), None);
        assert_eq!(ContentHash::from_hex(&format!("{}g", &text[1..])), None);
        assert_eq!(ContentHash::from_hex(""), None);
        assert!(serde_json::from_str::<ContentHash>("\"abc\"").is_err());
        assert!("zz".parse::<ContentHash>().is_err());
    }

    #[test]
    fn different_contents_have_different_hashes() {
        assert_ne!(ContentHash::of(b"a"), ContentHash::of(b"b"));
        assert_ne!(ContentHash::of(b""), ContentHash::of(b"\0"));
        assert_eq!(ContentHash::of(b"same"), ContentHash::of(b"same"));
    }
}
