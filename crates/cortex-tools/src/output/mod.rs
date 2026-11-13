//! The output policy: the one step every tool result passes before it
//! enters the context (`docs/architecture/05-tools.md`, "One output
//! policy").
//!
//! What it promises:
//!
//! - **It never cuts inside a line.** Whole lines are kept or left out. The
//!   one exception is announced on the line itself: a line longer than the
//!   line limit is shortened, and ends with a marker that says by how much.
//! - **It always says what was cut.** Lines left out are replaced, where
//!   they were, by a marker with their number, and the result ends with a
//!   notice of the totals.
//! - **The artifact holds everything.** When anything was lost, the output
//!   as the tool produced it is stored whole, and the notice gives the
//!   artifact's id, which `read` and `search` accept.
//! - **It happens once.** A result that has passed is within the limits, so
//!   passing it again changes nothing, and nothing shortens it later.
//!
//! There are two ways through it. A tool that knows its output shapes it
//! itself, with a [`Budget`], and writes a notice that fits what it does:
//! `read` gives the next `offset`, `search` says how many more matches
//! there are. Whatever a tool returns, [`execute`](crate::execute) then
//! checks it against the limits, and shapes what does not fit with
//! [`shape`], which is how the output of extension and MCP tools is
//! bounded.

mod compress;

use std::borrow::Cow;

use cortex_base::text::graphemes;
use cortex_protocol::item::ArtifactRef;
use cortex_protocol::message::Part;

use crate::context::ImageSupport;
use crate::result::ToolResult;
use crate::store::ContentStore;

/// The smallest limits that still leave room for a notice and a few lines.
const MIN_BYTES: usize = 4096;
const MIN_LINES: usize = 8;
/// A closing notice is one line of up to this many characters, so no line
/// limit may be lower.
const MIN_LINE_CHARS: usize = 256;

/// What the closing notice may take of the limits.
const NOTICE_LINES: usize = 1;
const NOTICE_BYTES: usize = 640;

/// How much of a store's error message a notice quotes.
const NOTICE_PROBLEM_CHARS: usize = 60;

/// What one marker for left-out lines may take.
const GAP_BYTES: usize = 48;

/// The limits on the model-visible content of one result.
///
/// The defaults are those of `docs/architecture/05-tools.md`: 32 KiB, 800
/// lines, 2,000 characters a line. Whichever is reached first applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLimits {
    max_bytes: usize,
    max_lines: usize,
    max_line_chars: usize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_bytes: 32 * 1024,
            max_lines: 800,
            max_line_chars: 2000,
        }
    }
}

impl OutputLimits {
    /// Limits from configuration. Values too small to work with are raised:
    /// at least 4 KiB, 8 lines and 256 characters a line. And the line limit
    /// is lowered until one line can take at most a quarter of the bytes, so
    /// that the limits can always be met by leaving out whole lines.
    #[must_use]
    pub fn new(max_bytes: usize, max_lines: usize, max_line_chars: usize) -> Self {
        let max_bytes = max_bytes.max(MIN_BYTES);
        Self {
            max_bytes,
            max_lines: max_lines.max(MIN_LINES),
            // A character is at most four bytes.
            max_line_chars: max_line_chars
                .clamp(MIN_LINE_CHARS, (max_bytes / 16).max(MIN_LINE_CHARS)),
        }
    }

    /// The most bytes a result's text may have.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// The most lines a result's text may have.
    #[must_use]
    pub fn max_lines(&self) -> usize {
        self.max_lines
    }

    /// The most characters one line may have.
    #[must_use]
    pub fn max_line_chars(&self) -> usize {
        self.max_line_chars
    }

    /// Whether `text` is within all three limits.
    #[must_use]
    pub fn fits(&self, text: &str) -> bool {
        if text.len() > self.max_bytes {
            return false;
        }
        let mut lines = 0;
        for line in text.split('\n') {
            lines += 1;
            if lines > self.max_lines || self.is_long(line) {
                return false;
            }
        }
        true
    }

    fn is_long(&self, line: &str) -> bool {
        // A line has at least as many bytes as characters, so most lines
        // are settled without counting.
        line.len() > self.max_line_chars && line.chars().count() > self.max_line_chars
    }

    /// Shortens a line that is over the line limit, ending it with a marker
    /// that says how much is missing. The result, marker included, is within
    /// the limit. The cut never splits a grapheme cluster. A line within the
    /// limit is returned as it is.
    #[must_use]
    pub fn clip<'a>(&self, line: &'a str) -> Cow<'a, str> {
        if !self.is_long(line) {
            return Cow::Borrowed(line);
        }
        Cow::Owned(self.clip_start(line, line.chars().count()))
    }

    /// [`clip`](Self::clip) for a line of which only the beginning is at
    /// hand: `start` is how the line begins, and `total_chars` is how many
    /// characters the whole line has. For a reader that does not keep the
    /// whole of a very long line in memory.
    #[must_use]
    pub fn clip_start(&self, start: &str, total_chars: usize) -> String {
        if total_chars <= self.max_line_chars && start.chars().count() >= total_chars {
            return start.to_owned();
        }
        // The marker's length depends on the number in it, which depends on
        // where the cut is. Sizing it for the whole line is always enough.
        let room = self
            .max_line_chars
            .saturating_sub(clip_marker(total_chars).chars().count());
        let mut kept_chars = 0;
        let mut end = 0;
        for (offset, grapheme, _) in graphemes(start) {
            let chars = grapheme.chars().count();
            if kept_chars + chars > room {
                break;
            }
            kept_chars += chars;
            end = offset + grapheme.len();
        }
        let mut out = String::with_capacity(end + 48);
        out.push_str(&start[..end]);
        out.push_str(&clip_marker(total_chars.saturating_sub(kept_chars)));
        out
    }

    /// A budget of these limits less what the caller sets aside for the
    /// lines it adds itself: a header, a closing notice.
    #[must_use]
    pub fn budget(&self, reserved_lines: usize, reserved_bytes: usize) -> Budget {
        Budget {
            limits: *self,
            lines_left: self.max_lines.saturating_sub(reserved_lines),
            bytes_left: self.max_bytes.saturating_sub(reserved_bytes),
        }
    }
}

/// The end of a line that was shortened.
fn clip_marker(missing: usize) -> String {
    format!("… [+{} characters]", count(missing))
}

/// What is left of the limits while a tool builds its output line by line.
///
/// A tool that shapes its own output takes lines from a budget until one
/// does not fit, and then says, in its own words, what it left out.
#[derive(Debug, Clone)]
pub struct Budget {
    limits: OutputLimits,
    lines_left: usize,
    bytes_left: usize,
}

impl Budget {
    /// Takes room for one line, with its line break. `false`, and nothing
    /// taken, when the line does not fit. The line is counted as
    /// [`OutputLimits::clip`] would leave it.
    pub fn take(&mut self, line: &str) -> bool {
        let bytes = self.limits.clip(line).len() + 1;
        if self.lines_left == 0 || bytes > self.bytes_left {
            return false;
        }
        self.lines_left -= 1;
        self.bytes_left -= bytes;
        true
    }

    /// The limits this budget was taken from.
    #[must_use]
    pub fn limits(&self) -> &OutputLimits {
        &self.limits
    }

    /// How many more lines fit.
    #[must_use]
    pub fn lines_left(&self) -> usize {
        self.lines_left
    }

    /// How many more bytes fit.
    #[must_use]
    pub fn bytes_left(&self) -> usize {
        self.bytes_left
    }
}

/// Which lines survive when output must be cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keep {
    /// The beginning.
    Head,
    /// The beginning and the end, a quarter and three quarters: where a
    /// command says what it is doing, and where it says how it went.
    HeadAndTail,
    /// The lines that report a problem, each with the lines around it, then
    /// the end and the beginning. For the output of builds and test runs.
    Important,
}

/// Whether output is compressed before the limits apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compression {
    /// The text is kept as it is: for content that was asked for.
    None,
    /// Control sequences are removed, redrawn lines collapse to their final
    /// state and runs of repeated lines collapse to a count: for output
    /// that was produced.
    Structural,
}

/// One piece of shaped output, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// A line that was kept. `source` is its index among the lines of the
    /// output after compression, counted from 0, and `clipped` says that it
    /// was over the line limit and was shortened.
    Line {
        /// Which line it is.
        source: usize,
        /// Whether it was shortened.
        clipped: bool,
    },
    /// A marker standing where this many lines were left out.
    Gap {
        /// How many lines it stands for.
        lines: usize,
    },
}

/// Output after the policy: the text to show, and an exact account of what
/// was done to get it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shaped {
    /// The text: one line for each [`Piece`], joined by line breaks.
    pub text: String,
    /// What each line of `text` is.
    pub pieces: Vec<Piece>,
    /// How many lines the output had after compression.
    pub total_lines: usize,
    /// How many of them were left out.
    pub omitted_lines: usize,
    /// How many bytes the left-out lines had.
    pub omitted_bytes: usize,
    /// How many kept lines were shortened.
    pub clipped_lines: usize,
    /// How many identical lines were replaced by a count. Nothing was lost.
    pub repeated_lines: usize,
    /// How many similar lines were replaced by a count. Their text was
    /// lost.
    pub similar_lines: usize,
}

impl Shaped {
    /// Whether anything of the output is missing from `text`: lines left
    /// out, lines shortened, or similar lines folded away. Removed control
    /// sequences and counted repeats do not count.
    #[must_use]
    pub fn is_lossy(&self) -> bool {
        self.omitted_lines > 0 || self.clipped_lines > 0 || self.similar_lines > 0
    }

    /// The closing notice for output that lost something: what was cut, and
    /// where the whole of it is. `None` when nothing was lost.
    #[must_use]
    pub fn notice(
        &self,
        original_bytes: usize,
        kept: Result<&ArtifactRef, &str>,
    ) -> Option<String> {
        if !self.is_lossy() {
            return None;
        }
        let mut what = Vec::new();
        if self.omitted_lines > 0 {
            what.push(format!(
                "{} of {} lines not shown",
                count(self.omitted_lines),
                count(self.total_lines)
            ));
        }
        if self.similar_lines > 0 {
            what.push(format!(
                "{} similar lines folded",
                count(self.similar_lines)
            ));
        }
        if self.clipped_lines > 0 {
            what.push(format!(
                "{} long lines shortened",
                count(self.clipped_lines)
            ));
        }
        let size = size(original_bytes);
        Some(match kept {
            Ok(artifact) => format!(
                "[Output cut: {}. The whole output ({size}) is artifact {}: pass that id as \
                 `artifact` to read or search.]",
                what.join(", "),
                artifact.id
            ),
            Err(problem) => format!(
                "[Output cut: {}. The whole output ({size}) could not be kept: {}]",
                what.join(", "),
                // One line of the problem, short enough for the notice.
                problem
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(NOTICE_PROBLEM_CHARS)
                    .collect::<String>()
            ),
        })
    }
}

/// Applies the policy to `text`: compression, then the limits.
///
/// The result leaves room for the closing notice, so that
/// [`Shaped::text`] with [`Shaped::notice`] after it is within `limits`.
#[must_use]
pub fn shape(text: &str, limits: &OutputLimits, keep: Keep, compression: Compression) -> Shaped {
    // A final line break ends the last line; it does not start an empty one.
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut lines: Vec<Cow<'_, str>> = Vec::new();
    let mut repeated_lines = 0;
    let mut similar_lines = 0;
    if !text.is_empty() {
        match compression {
            Compression::None => lines.extend(body.split('\n').map(Cow::Borrowed)),
            Compression::Structural => {
                let cleaned = match compress::strip_control(body) {
                    Cow::Borrowed(clean) => clean
                        .split('\n')
                        .map(|line| Cow::Borrowed(compress::final_state(line)))
                        .collect(),
                    Cow::Owned(clean) => clean
                        .split('\n')
                        .map(|line| Cow::Owned(compress::final_state(line).to_owned()))
                        .collect(),
                };
                (lines, repeated_lines) = compress::collapse_repeats(cleaned);
                // Folding similar lines loses their text, so it is done only
                // to output that is over the limits and would be cut anyway.
                if !within(&lines, limits) {
                    (lines, similar_lines) = compress::collapse_similar(lines);
                }
            }
        }
    }

    let clipped: Vec<Cow<'_, str>> = lines.iter().map(|line| limits.clip(line)).collect();
    let any_clipped = clipped.iter().any(|line| matches!(line, Cow::Owned(_)));
    let kept = if similar_lines == 0 && !any_clipped && within(&clipped, limits) {
        // Nothing is lost, so there is no notice to leave room for.
        vec![true; clipped.len()]
    } else {
        select(&clipped, limits, keep)
    };

    let mut out: Vec<Cow<'_, str>> = Vec::new();
    let mut pieces = Vec::new();
    let mut omitted_lines = 0;
    let mut omitted_bytes = 0;
    let mut clipped_lines = 0;
    let mut index = 0;
    while index < clipped.len() {
        if kept[index] {
            let is_clipped = matches!(clipped[index], Cow::Owned(_));
            clipped_lines += usize::from(is_clipped);
            pieces.push(Piece::Line {
                source: index,
                clipped: is_clipped,
            });
            out.push(Cow::Borrowed(clipped[index].as_ref()));
            index += 1;
        } else {
            let start = index;
            while index < clipped.len() && !kept[index] {
                omitted_bytes += lines[index].len() + 1;
                index += 1;
            }
            omitted_lines += index - start;
            pieces.push(Piece::Gap {
                lines: index - start,
            });
            out.push(Cow::Owned(gap_marker(index - start)));
        }
    }

    Shaped {
        text: out.join("\n"),
        pieces,
        total_lines: lines.len(),
        omitted_lines,
        omitted_bytes,
        clipped_lines,
        repeated_lines,
        similar_lines,
    }
}

/// Whether lines, joined by line breaks, are within the byte and line
/// limits. Line length is not looked at.
fn within(lines: &[Cow<'_, str>], limits: &OutputLimits) -> bool {
    lines.len() <= limits.max_lines
        && lines.iter().map(|line| line.len() + 1).sum::<usize>() <= limits.max_bytes + 1
}

/// The line that stands where `lines` lines were left out.
fn gap_marker(lines: usize) -> String {
    let noun = if lines == 1 { "line" } else { "lines" };
    format!("[… {} {noun} not shown …]", count(lines))
}

/// Chooses which lines to keep, within the limits less the room for the
/// closing notice, counting a marker for every stretch that is left out.
fn select(lines: &[Cow<'_, str>], limits: &OutputLimits, keep: Keep) -> Vec<bool> {
    let mut chosen = Chosen {
        lines,
        kept: vec![false; lines.len()],
        // With nothing kept yet, one marker stands for everything.
        lines_left: limits.max_lines.saturating_sub(NOTICE_LINES + 1),
        bytes_left: limits.max_bytes.saturating_sub(NOTICE_BYTES + GAP_BYTES),
    };
    let count = lines.len();
    match keep {
        Keep::Head => {
            chosen.run(0..count);
        }
        Keep::HeadAndTail => {
            let head_lines = chosen.lines_left / 4;
            let head_bytes = chosen.bytes_left / 4;
            let (all_lines, all_bytes) = (chosen.lines_left, chosen.bytes_left);
            // The head gets a quarter. What it does not use, the tail may.
            chosen.lines_left = head_lines;
            chosen.bytes_left = head_bytes;
            chosen.run(0..count);
            chosen.lines_left += all_lines - head_lines;
            chosen.bytes_left += all_bytes - head_bytes;
            chosen.run((0..count).rev());
            chosen.run(0..count);
        }
        Keep::Important => {
            // Always a little of the end and of the beginning.
            chosen.run((count.saturating_sub(5)..count).rev());
            chosen.run(0..count.min(5));
            // Then the lines that report a problem, each with the line
            // before it and the two after, in three quarters of the room.
            let floor_lines = chosen.lines_left / 4;
            let floor_bytes = chosen.bytes_left / 4;
            for (index, line) in lines.iter().enumerate() {
                if chosen.lines_left <= floor_lines || chosen.bytes_left <= floor_bytes {
                    break;
                }
                if compress::is_important(line) {
                    for near in index.saturating_sub(1)..(index + 3).min(count) {
                        chosen.add(near);
                    }
                }
            }
            chosen.run((0..count).rev());
            chosen.run(0..count);
        }
    }
    chosen.kept
}

struct Chosen<'a> {
    lines: &'a [Cow<'a, str>],
    kept: Vec<bool>,
    lines_left: usize,
    bytes_left: usize,
}

impl Chosen<'_> {
    /// Keeps line `index` if there is room for it and for the markers it
    /// brings. A line in the middle of a left-out stretch splits the stretch
    /// in two, which is one marker more. A line that closes a stretch takes
    /// its marker away again.
    fn add(&mut self, index: usize) -> bool {
        if self.kept[index] {
            return true;
        }
        let left_out = |at: Option<usize>| {
            at.and_then(|at| self.kept.get(at))
                .is_some_and(|kept| !kept)
        };
        // How many markers the stretch around this line needs once the line
        // is kept. It needed one.
        let markers =
            usize::from(left_out(index.checked_sub(1))) + usize::from(left_out(Some(index + 1)));
        let lines_after = self.lines_left + 1;
        let bytes_after = self.bytes_left + GAP_BYTES;
        let lines = 1 + markers;
        let bytes = self.lines[index].len() + 1 + markers * GAP_BYTES;
        if lines > lines_after || bytes > bytes_after {
            return false;
        }
        self.lines_left = lines_after - lines;
        self.bytes_left = bytes_after - bytes;
        self.kept[index] = true;
        true
    }

    /// Keeps lines in the order given, past the ones already kept, until
    /// one does not fit.
    fn run(&mut self, order: impl Iterator<Item = usize>) {
        for index in order {
            if !self.add(index) {
                break;
            }
        }
    }
}

/// How the policy treats the text of a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Treatment {
    /// The text is what a tool of the harness wrote. It is checked against
    /// the limits and left exactly as it is when it fits: a file read must
    /// reach the model unchanged.
    AsReturned,
    /// The text was produced outside the harness, by an extension or MCP
    /// tool. It is compressed whether or not it fits: control sequences
    /// removed, redrawn lines collapsed, repeated lines counted.
    Produced,
}

/// Applies the policy to a result: what [`execute`](crate::execute) does to
/// whatever a tool returned.
///
/// - Text within the limits passes: as it is, or compressed, by `treatment`.
/// - Text over them is shaped as produced output (compressed, beginning and
///   end kept), the whole of it goes to an artifact, and the closing notice
///   names the artifact.
/// - An image the model cannot take (no image support, a type it does not
///   read, too large) is replaced by a line saying so.
/// - A result holds text and images. Any other kind of part is left out,
///   with a line saying so, and a provider's raw block on a text part is
///   dropped: neither is counted by the limits, so neither may pass.
///
/// It blocks when it has to store an artifact.
#[must_use]
pub fn enforce(
    result: ToolResult,
    tool: &str,
    limits: &OutputLimits,
    images: Option<&ImageSupport>,
    store: &dyn ContentStore,
    treatment: Treatment,
) -> ToolResult {
    if result.is_bounded() {
        return result;
    }
    let mut dropped = Vec::new();
    let mut kept_images = Vec::new();
    let mut texts = 0;
    let mut plain = true;
    let mut foreign = 0;
    for part in result.content() {
        match part {
            Part::Image(image) => {
                // Base64 writes three bytes as four characters.
                let bytes = image.data.as_ref().map(|data| data.len() as u64 / 4 * 3);
                let accepted = match (images, bytes) {
                    (Some(images), Some(bytes)) => images.accepts(&image.media_type, bytes),
                    // An image held as an artifact was sized by the tool
                    // that stored it.
                    (Some(images), None) => images.accepts(&image.media_type, 0),
                    (None, _) => false,
                };
                if accepted {
                    kept_images.push(part.clone());
                } else {
                    let size = bytes
                        .map_or_else(String::new, |bytes| format!(", {}", size(bytes as usize)));
                    dropped.push(format!(
                        "[An image was left out ({}{size}): the model in use does not take it.]",
                        image.media_type
                    ));
                }
            }
            Part::Text(text) => {
                texts += 1;
                plain &= text.raw.is_none();
            }
            _ => foreign += 1,
        }
    }
    if foreign > 0 {
        dropped.push(format!(
            "[{} of this result {} left out: a tool result holds text and images only.]",
            if foreign == 1 {
                "A part".to_owned()
            } else {
                format!("{} parts", count(foreign))
            },
            if foreign == 1 { "was" } else { "were" }
        ));
    }

    let mut text = result.text();
    for line in &dropped {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    }
    if treatment == Treatment::AsReturned && limits.fits(&text) {
        if dropped.is_empty() && texts <= 1 && plain {
            let content = result.content().to_vec();
            return result.bounded_with(content, None);
        }
        let mut content = vec![Part::text(text)];
        content.extend(kept_images);
        return result.bounded_with(content, None);
    }

    let shaped = shape(&text, limits, Keep::HeadAndTail, Compression::Structural);
    let mut out = shaped.text.clone();
    let mut artifact = None;
    if shaped.is_lossy() {
        let stored = store.put_artifact(
            &format!("full output of {tool}"),
            "text/plain",
            text.as_bytes(),
        );
        let notice = match &stored {
            Ok(reference) => shaped.notice(text.len(), Ok(reference)),
            Err(error) => shaped.notice(text.len(), Err(&error.to_string())),
        };
        if let Some(notice) = notice {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&notice);
        }
        artifact = stored.ok();
    }
    let mut content = vec![Part::text(out)];
    content.extend(kept_images);
    result.bounded_with(content, artifact)
}

/// A whole number with a comma between its thousands: `12,345`.
pub(crate) fn count(number: usize) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A size in bytes for people: `512 bytes`, `31.5 KiB`, `1.2 MiB`.
pub(crate) fn size(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    const GIB: usize = 1024 * 1024 * 1024;
    let tenths = |unit: usize| {
        let tenths = (bytes as u128 * 10 + unit as u128 / 2) / unit as u128;
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    match bytes {
        1 => "1 byte".to_owned(),
        bytes if bytes < KIB => format!("{bytes} bytes"),
        bytes if bytes < MIB => format!("{} KiB", tenths(KIB)),
        bytes if bytes < GIB => format!("{} MiB", tenths(MIB)),
        _ => format!("{} GiB", tenths(GIB)),
    }
}

#[cfg(test)]
mod tests;
