//! What is kept of a command's output: a bounded buffer, and an artifact
//! for the rest (`docs/architecture/05-tools.md`, "`shell`", "Output").
//!
//! - **The buffer is bounded.** It holds the first bytes and the last
//!   bytes, 8 KiB and 24 KiB unless told otherwise, however much a command
//!   writes.
//! - **Once output passes the buffer, all of it goes to an artifact,** from
//!   its first byte, written piece by piece as it arrives.
//! - **What is shown is whole lines.** The kept beginning ends at a line
//!   break and the kept end starts after one, with a marker between them
//!   that says how much is missing.

use std::collections::VecDeque;
use std::io::Write;

use cortex_protocol::item::ArtifactRef;

use crate::output::{count, size};
use crate::store::{ArtifactWriter, ContentStore};

/// How many bytes of the beginning are kept.
pub(crate) const HEAD_BYTES: usize = 8 * 1024;
/// How many bytes of the end are kept.
pub(crate) const TAIL_BYTES: usize = 24 * 1024;

/// The output of a command while it arrives.
pub(crate) struct Capture {
    head_room: usize,
    tail_room: usize,
    /// Everything so far, until the output passes the buffer; from then on
    /// its beginning.
    head: Vec<u8>,
    /// The end, once the output has passed the buffer.
    tail: VecDeque<u8>,
    spilled: bool,
    total_bytes: u64,
    total_breaks: u64,
    label: String,
    artifact: Option<Box<dyn ArtifactWriter>>,
    artifact_problem: Option<String>,
}

/// The output of a command that has ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    /// The beginning: all of the output when nothing was dropped.
    pub head: Vec<u8>,
    /// The end, when the middle was dropped. Empty otherwise.
    pub tail: Vec<u8>,
    /// How many bytes the command wrote.
    pub total_bytes: u64,
    /// How many line breaks were among them.
    pub total_breaks: u64,
    /// The artifact that holds all of it, when the output passed the
    /// buffer: the reference, or why it could not be kept.
    pub artifact: Option<Result<ArtifactRef, String>>,
}

impl Capture {
    pub(crate) fn new(label: &str) -> Self {
        Self::with_room(label, HEAD_BYTES, TAIL_BYTES)
    }

    pub(crate) fn with_room(label: &str, head_room: usize, tail_room: usize) -> Self {
        Self {
            head_room,
            tail_room,
            head: Vec::new(),
            tail: VecDeque::new(),
            spilled: false,
            total_bytes: 0,
            total_breaks: 0,
            label: label.to_owned(),
            artifact: None,
            artifact_problem: None,
        }
    }

    /// Takes the next bytes of output. It blocks when it writes to the
    /// artifact, so it is called from the thread that reads the pipe.
    pub(crate) fn push(&mut self, bytes: &[u8], store: &dyn ContentStore) {
        self.total_bytes += bytes.len() as u64;
        self.total_breaks += bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
        if !self.spilled {
            if self.head.len() + bytes.len() <= self.head_room + self.tail_room {
                self.head.extend_from_slice(bytes);
                return;
            }
            // The output has passed the buffer. Everything so far is still
            // here, so the artifact starts from the first byte.
            self.spilled = true;
            match store.begin_artifact(&self.label, "text/plain") {
                Ok(writer) => self.artifact = Some(writer),
                Err(error) => self.artifact_problem = Some(error.to_string()),
            }
            let so_far = std::mem::take(&mut self.head);
            self.write(&so_far);
            let keep = so_far.len().min(self.head_room);
            self.head.extend_from_slice(&so_far[..keep]);
            self.keep_end(&so_far[keep..]);
        }
        self.write(bytes);
        // The beginning may still have room, when the buffer's two halves
        // were filled by one large piece.
        let room = self.head_room.saturating_sub(self.head.len());
        let (to_head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(to_head);
        self.keep_end(rest);
    }

    fn keep_end(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.tail_room {
            self.tail.clear();
            self.tail.extend(&bytes[bytes.len() - self.tail_room..]);
            return;
        }
        let over = (self.tail.len() + bytes.len()).saturating_sub(self.tail_room);
        self.tail.drain(..over);
        self.tail.extend(bytes);
    }

    fn write(&mut self, bytes: &[u8]) {
        if let Some(writer) = &mut self.artifact
            && let Err(error) = writer.write_all(bytes)
        {
            // Half an artifact is not the whole output. Give it up, and
            // say why nothing was kept.
            self.artifact_problem = Some(format!("the content store failed: {error}"));
            self.artifact = None;
        }
    }

    /// Ends the capture, and completes the artifact if there is one.
    pub(crate) fn finish(self) -> Captured {
        let artifact = match (self.artifact, self.artifact_problem) {
            (Some(writer), _) => Some(writer.finish().map_err(|error| error.to_string())),
            (None, Some(problem)) => Some(Err(problem)),
            (None, None) => None,
        };
        Captured {
            head: self.head,
            tail: self.tail.into(),
            total_bytes: self.total_bytes,
            total_breaks: self.total_breaks,
            artifact,
        }
    }
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("total_bytes", &self.total_bytes)
            .field("spilled", &self.spilled)
            .finish_non_exhaustive()
    }
}

/// The kept output as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptText {
    /// The text: all of the output, or its beginning, a marker line and its
    /// end.
    pub text: String,
    /// How many lines the marker stands for. 0 when nothing was dropped.
    pub dropped_lines: u64,
    /// How many bytes the marker stands for.
    pub dropped_bytes: u64,
}

impl Captured {
    /// Whether the middle of the output was dropped from the buffer.
    #[must_use]
    pub fn is_cut(&self) -> bool {
        self.total_bytes > (self.head.len() + self.tail.len()) as u64
    }

    /// Every byte the command wrote, when the buffer still holds them all.
    #[must_use]
    pub fn whole(&self) -> Option<&[u8]> {
        (!self.is_cut() && self.tail.is_empty()).then_some(self.head.as_slice())
    }

    /// The kept output as text, cut at line breaks, with a marker where the
    /// middle is missing. Bytes that are not UTF-8 are replaced.
    #[must_use]
    pub fn text(&self) -> KeptText {
        if !self.is_cut() {
            let mut all = self.head.clone();
            all.extend_from_slice(&self.tail);
            return KeptText {
                text: String::from_utf8_lossy(&all).into_owned(),
                dropped_lines: 0,
                dropped_bytes: 0,
            };
        }
        // Whole lines only: the beginning up to its last line break, the
        // end from after its first. A side with no line break in it is one
        // piece of a very long line, and is kept as it is.
        let head = match self.head.iter().rposition(|byte| *byte == b'\n') {
            Some(last) => &self.head[..=last],
            None => &self.head[..],
        };
        let tail = match self.tail.iter().position(|byte| *byte == b'\n') {
            Some(first) => &self.tail[first + 1..],
            None => &self.tail[..],
        };
        let breaks = |bytes: &[u8]| bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
        let dropped_bytes = self.total_bytes - (head.len() + tail.len()) as u64;
        // What is between the two kept pieces ends with a line break when
        // the end starts after one, and that break ends its last line.
        let dropped_lines = self
            .total_breaks
            .saturating_sub(breaks(head) + breaks(tail));
        let marker = if dropped_lines > 0 {
            let noun = if dropped_lines == 1 { "line" } else { "lines" };
            format!(
                "[… {} {noun} not shown …]",
                count(usize::try_from(dropped_lines).unwrap_or(usize::MAX))
            )
        } else {
            format!(
                "[… {} not shown …]",
                size(usize::try_from(dropped_bytes).unwrap_or(usize::MAX))
            )
        };
        let mut text = String::from_utf8_lossy(head).into_owned();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&marker);
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(tail));
        KeptText {
            text,
            dropped_lines,
            dropped_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;
    use cortex_testkit::cases::{self, ensure};

    use super::*;
    use crate::store::MemoryStore;

    fn capture(pieces: &[&[u8]], head: usize, tail: usize) -> (Captured, MemoryStore) {
        let store = MemoryStore::new();
        let mut capture = Capture::with_room("full output of the test", head, tail);
        for piece in pieces {
            capture.push(piece, &store);
        }
        (capture.finish(), store)
    }

    #[test]
    fn output_that_fits_is_kept_whole_and_makes_no_artifact() {
        let (kept, store) = capture(&[b"one\n", b"two\n", b"three"], 8, 8);
        assert_eq!(kept.whole(), Some(&b"one\ntwo\nthree"[..]));
        assert!(!kept.is_cut());
        assert_eq!(kept.total_bytes, 13);
        assert_eq!(kept.total_breaks, 2);
        assert_eq!(kept.artifact, None);
        assert_eq!(store.artifact_count(), 0);
        let text = kept.text();
        assert_eq!(text.text, "one\ntwo\nthree");
        assert_eq!((text.dropped_lines, text.dropped_bytes), (0, 0));

        // Exactly the buffer's size still fits.
        let (kept, store) = capture(&[b"12345678", b"abcdefgh"], 8, 8);
        assert_eq!(kept.whole(), Some(&b"12345678abcdefgh"[..]));
        assert_eq!(store.artifact_count(), 0);
    }

    #[test]
    fn output_past_the_buffer_keeps_both_ends_and_stores_all_of_it() {
        let lines: Vec<String> = (1..=100).map(|number| format!("line {number}\n")).collect();
        let pieces: Vec<&[u8]> = lines.iter().map(String::as_bytes).collect();
        let (kept, store) = capture(&pieces, 30, 40);
        let all: String = lines.concat();

        assert!(kept.is_cut());
        assert_eq!(kept.whole(), None);
        assert_eq!(kept.head, all.as_bytes()[..30]);
        assert_eq!(kept.tail, all.as_bytes()[all.len() - 40..]);
        assert_eq!(kept.total_bytes, all.len() as u64);
        assert_eq!(kept.total_breaks, 100);

        let reference = kept
            .artifact
            .clone()
            .expect("the output passed the buffer")
            .expect("the store kept it");
        assert_eq!(reference.label, "full output of the test");
        assert_eq!(reference.bytes, Some(all.len() as u64));
        assert_eq!(
            store.artifact(&reference.id).expect("stored"),
            all.as_bytes()
        );

        let text = kept.text();
        // Whole lines on both sides, and the count of what is between them.
        assert_eq!(
            text.text,
            "line 1\nline 2\nline 3\nline 4\n[… 92 lines not shown …]\n\
             line 97\nline 98\nline 99\nline 100\n"
        );
        assert_eq!(text.dropped_lines, 92);
        let shown = text.text.replace("[… 92 lines not shown …]\n", "");
        assert_eq!(text.dropped_bytes, (all.len() - shown.len()) as u64);
    }

    #[test]
    fn one_very_long_line_is_cut_by_size() {
        let line = vec![b'x'; 5000];
        let (kept, _store) = capture(&[&line], 100, 200);
        let text = kept.text();
        assert_eq!(text.dropped_lines, 0);
        assert_eq!(text.dropped_bytes, 4700);
        assert_eq!(
            text.text,
            format!(
                "{}\n[… 4.6 KiB not shown …]\n{}",
                "x".repeat(100),
                "x".repeat(200)
            )
        );
    }

    #[test]
    fn a_store_that_fails_is_reported_and_the_buffer_still_works() {
        let store = crate::store::Unavailable("the disk is full".to_owned());
        let mut capture = Capture::with_room("label", 4, 4);
        capture.push(b"0123456789abcdef", &store);
        capture.push(b"ghij", &store);
        let kept = capture.finish();
        assert_eq!(kept.head, b"0123");
        assert_eq!(kept.tail, b"ghij");
        let problem = kept
            .artifact
            .expect("the output passed the buffer")
            .expect_err("the store refused");
        assert!(problem.contains("the disk is full"), "{problem}");
    }

    /// However the output is cut into pieces, the buffer holds its first
    /// and last bytes, and the artifact holds all of it.
    #[test]
    fn any_chunking_keeps_the_same_ends_and_stores_everything() {
        cases::check(
            "capture_any_chunking",
            300,
            |rng| {
                let head = 1 + rng.below(40) as usize;
                let tail = 1 + rng.below(40) as usize;
                let length = rng.below(260) as usize;
                let bytes: Vec<u8> = (0..length)
                    .map(|_| {
                        if rng.below(6) == 0 {
                            b'\n'
                        } else {
                            b'a' + rng.below(26) as u8
                        }
                    })
                    .collect();
                let mut cuts = Vec::new();
                let mut at = 0;
                while at < bytes.len() {
                    at = (at + 1 + rng.below(90) as usize).min(bytes.len());
                    cuts.push(at);
                }
                (head, tail, bytes, cuts)
            },
            |(head, tail, bytes, cuts)| {
                let store = MemoryStore::new();
                let mut capture = Capture::with_room("all", *head, *tail);
                let mut from = 0;
                for cut in cuts {
                    capture.push(&bytes[from..*cut], &store);
                    from = *cut;
                }
                let kept = capture.finish();
                ensure!(
                    kept.total_bytes == bytes.len() as u64,
                    "the byte count is wrong"
                );
                if bytes.len() <= head + tail {
                    ensure!(
                        kept.whole() == Some(bytes.as_slice()) && kept.artifact.is_none(),
                        "output that fits was not kept whole"
                    );
                    return Ok(());
                }
                ensure!(
                    kept.head == bytes[..*head],
                    "the beginning is not the first bytes"
                );
                ensure!(
                    kept.tail == bytes[bytes.len() - tail..],
                    "the end is not the last bytes"
                );
                let Some(Ok(reference)) = &kept.artifact else {
                    return Err("there is no artifact".into());
                };
                ensure!(
                    store.artifact(&reference.id).as_deref() == Some(bytes.as_slice()),
                    "the artifact is not the whole output"
                );
                // The text is whole lines of the output around one marker,
                // and the marker's count is the lines that are missing.
                let text = kept.text();
                let all = String::from_utf8_lossy(bytes);
                let Some((before, after)) = text.text.split_once("[… ") else {
                    return Err("there is no marker".into());
                };
                let Some((_, after)) = after.split_once(" …]\n") else {
                    return Err("the marker does not end".into());
                };
                // A beginning with no line break in it is given one, to put
                // the marker on a line of its own.
                let added = usize::from(!before.is_empty() && !kept.head.contains(&b'\n'));
                let before_shown = if added == 1 {
                    before.strip_suffix('\n').unwrap_or(before)
                } else {
                    before
                };
                ensure!(
                    all.starts_with(before_shown),
                    "the text does not begin as the output does"
                );
                ensure!(
                    all.ends_with(after),
                    "the text does not end as the output does"
                );
                let shown = before_shown.matches('\n').count() + after.matches('\n').count();
                let missing = all.matches('\n').count() - shown;
                ensure!(
                    text.dropped_lines as usize == missing,
                    "the marker says {} lines and {missing} are missing",
                    text.dropped_lines
                );
                ensure!(
                    text.dropped_bytes as usize == all.len() - before_shown.len() - after.len(),
                    "the marker's bytes are wrong"
                );
                Ok(())
            },
        );
    }
}
