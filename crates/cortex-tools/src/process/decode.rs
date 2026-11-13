//! Bytes to text, a piece at a time.
//!
//! A command's output arrives in pieces that end wherever a read ended,
//! which may be in the middle of a character. [`Utf8Stream`] holds back the
//! bytes of a character that is not whole yet, so that what is streamed to
//! front ends is the text a single decoding of all the bytes would give.
//! Bytes that are not UTF-8 become the replacement character.

/// A decoder that remembers the incomplete character at the end of a piece.
#[derive(Debug, Default)]
pub(crate) struct Utf8Stream {
    /// The first bytes of a character whose rest has not arrived: at most 3.
    held: Vec<u8>,
}

impl Utf8Stream {
    /// Decodes the next piece, appending its text to `out`.
    pub(crate) fn push(&mut self, bytes: &[u8], out: &mut String) {
        let joined;
        let mut rest: &[u8] = if self.held.is_empty() {
            bytes
        } else {
            self.held.extend_from_slice(bytes);
            joined = std::mem::take(&mut self.held);
            &joined
        };
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    return;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    // The split is at a boundary the decoder itself found.
                    out.push_str(&String::from_utf8_lossy(valid));
                    match error.error_len() {
                        // Bytes that can never be a character.
                        Some(length) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[length..];
                        }
                        // The start of a character, cut off by the end of
                        // the piece.
                        None => {
                            self.held = after.to_vec();
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Ends the stream: a character that never became whole is replaced.
    pub(crate) fn finish(&mut self, out: &mut String) {
        if !self.held.is_empty() {
            self.held.clear();
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;
    use cortex_testkit::cases::{self, ensure};

    use super::*;

    fn decode(pieces: &[&[u8]]) -> String {
        let mut stream = Utf8Stream::default();
        let mut out = String::new();
        for piece in pieces {
            stream.push(piece, &mut out);
        }
        stream.finish(&mut out);
        out
    }

    #[test]
    fn a_character_split_between_two_pieces_arrives_whole() {
        let text = "naïve — 日本語 🙂";
        let bytes = text.as_bytes();
        for cut in 0..=bytes.len() {
            assert_eq!(
                decode(&[&bytes[..cut], &bytes[cut..]]),
                text,
                "cut at {cut}"
            );
        }
        // One byte at a time.
        let single: Vec<&[u8]> = bytes.chunks(1).collect();
        assert_eq!(decode(&single), text);
    }

    #[test]
    fn bytes_that_are_not_text_are_replaced_and_nothing_else_is_lost() {
        assert_eq!(decode(&[b"ok \xff\xfe end"]), "ok \u{fffd}\u{fffd} end");
        // The start of a character that never gets its rest.
        assert_eq!(decode(&[b"cut \xe6\x97"]), "cut \u{fffd}");
        assert_eq!(decode(&[b"cut \xe6", b"\x97", b"x"]), "cut \u{fffd}x");
        assert_eq!(decode(&[]), "");
    }

    /// Whatever the bytes and however they are cut, the stream gives what
    /// one lossy decoding of all of them gives.
    #[test]
    fn any_chunking_decodes_like_the_whole() {
        cases::check(
            "utf8_stream_any_chunking",
            300,
            |rng| {
                let alphabet: [&[u8]; 8] = [
                    b"a",
                    b"\n",
                    "é".as_bytes(),
                    "日".as_bytes(),
                    "🙂".as_bytes(),
                    b"\xff",
                    b"\xe6\x97",
                    b"\xf0\x9f",
                ];
                let mut bytes = Vec::new();
                for _ in 0..rng.below(24) {
                    bytes.extend_from_slice(alphabet[rng.below(8) as usize]);
                }
                let mut cuts = Vec::new();
                let mut at = 0;
                while at < bytes.len() {
                    at = (at + 1 + rng.below(5) as usize).min(bytes.len());
                    cuts.push(at);
                }
                (bytes, cuts)
            },
            |(bytes, cuts)| {
                let mut pieces = Vec::new();
                let mut from = 0;
                for cut in cuts {
                    pieces.push(&bytes[from..*cut]);
                    from = *cut;
                }
                let streamed = decode(&pieces);
                let whole = String::from_utf8_lossy(bytes);
                ensure!(streamed == whole, "streamed {streamed:?}, whole {whole:?}");
                Ok(())
            },
        );
    }
}
