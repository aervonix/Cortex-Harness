//! Telling text from binary, and naming what a binary file is.
//!
//! `read` refuses a binary file with its size and detected type, and returns
//! an image as an image where the model takes one
//! (`docs/architecture/05-tools.md`, "`read`").

/// How much of a file's start is looked at.
pub(crate) const SNIFF_BYTES: usize = 8 * 1024;

/// What the start of a file says the file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Text, as far as its start shows.
    Text,
    /// An image, with its media type.
    Image(&'static str),
    /// Something else that is not text, with a name for it.
    Binary(&'static str),
}

/// Formats recognised by how they begin.
const MAGIC: &[(&[u8], Kind)] = &[
    (b"\x89PNG\r\n\x1a\n", Kind::Image("image/png")),
    (b"\xff\xd8\xff", Kind::Image("image/jpeg")),
    (b"GIF87a", Kind::Image("image/gif")),
    (b"GIF89a", Kind::Image("image/gif")),
    (b"%PDF-", Kind::Binary("a PDF document")),
    (b"PK\x03\x04", Kind::Binary("a ZIP archive")),
    (b"\x1f\x8b", Kind::Binary("gzip-compressed data")),
    (b"\x7fELF", Kind::Binary("an ELF executable")),
    (b"MZ", Kind::Binary("a Windows executable")),
    (b"\x00asm", Kind::Binary("a WebAssembly module")),
    (b"SQLite format 3\x00", Kind::Binary("an SQLite database")),
    (b"\xff\xfe", Kind::Binary("UTF-16 text")),
    (b"\xfe\xff", Kind::Binary("UTF-16 text")),
];

/// Looks at the first bytes of a file.
///
/// A known signature decides. Failing that, a NUL byte means binary: text
/// in UTF-8 has none, and nearly every binary format has one early.
pub(crate) fn sniff(start: &[u8]) -> Kind {
    let start = &start[..start.len().min(SNIFF_BYTES)];
    for (magic, kind) in MAGIC {
        if start.starts_with(magic) {
            // `MZ` and the UTF-16 marks are two bytes, which text could
            // begin with by chance. They count only with a NUL after them.
            if magic.len() > 2 || start.contains(&0) {
                return *kind;
            }
        }
    }
    if start.len() >= 12 && &start[..4] == b"RIFF" && &start[8..12] == b"WEBP" {
        return Kind::Image("image/webp");
    }
    if start.contains(&0) {
        return Kind::Binary("binary data");
    }
    Kind::Text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_known_by_their_first_bytes() {
        assert_eq!(
            sniff(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Kind::Image("image/png")
        );
        assert_eq!(
            sniff(b"\xff\xd8\xff\xe0\0\x10JFIF"),
            Kind::Image("image/jpeg")
        );
        assert_eq!(sniff(b"GIF89a\x01\0\x01\0"), Kind::Image("image/gif"));
        assert_eq!(sniff(b"RIFF\x24\0\0\0WEBPVP8 "), Kind::Image("image/webp"));
        assert_eq!(
            sniff(b"RIFF\x24\0\0\0WAVEfmt "),
            Kind::Binary("binary data")
        );
    }

    #[test]
    fn other_binary_formats_are_named() {
        assert_eq!(sniff(b"%PDF-1.7\n"), Kind::Binary("a PDF document"));
        assert_eq!(sniff(b"PK\x03\x04\x14\0"), Kind::Binary("a ZIP archive"));
        assert_eq!(
            sniff(b"\x7fELF\x02\x01\x01\0"),
            Kind::Binary("an ELF executable")
        );
        assert_eq!(
            sniff(b"MZ\x90\0\x03\0"),
            Kind::Binary("a Windows executable")
        );
        assert_eq!(sniff(b"\xff\xfeh\0i\0"), Kind::Binary("UTF-16 text"));
        assert_eq!(sniff(b"\x01\x02\0\x03"), Kind::Binary("binary data"));
    }

    #[test]
    fn text_is_text_even_when_it_starts_like_a_format() {
        assert_eq!(sniff(b""), Kind::Text);
        assert_eq!(sniff(b"fn main() {}\n"), Kind::Text);
        assert_eq!(
            sniff("MZ is how this sentence begins".as_bytes()),
            Kind::Text
        );
        assert_eq!(
            sniff("\u{feff}a file with a byte-order mark".as_bytes()),
            Kind::Text
        );
        assert_eq!(sniff("naïve café 日本語".as_bytes()), Kind::Text);
    }

    #[test]
    fn only_the_start_of_a_file_is_looked_at() {
        let mut late = vec![b'a'; SNIFF_BYTES];
        late.push(0);
        assert_eq!(sniff(&late), Kind::Text);
        late[SNIFF_BYTES - 1] = 0;
        assert_eq!(sniff(&late), Kind::Binary("binary data"));
    }
}
