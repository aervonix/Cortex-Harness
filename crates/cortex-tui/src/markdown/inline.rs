//! Inline markdown: code spans, emphasis and strong emphasis, links and
//! autolinks, backslash escapes, entities and hard line breaks, parsed from
//! the text of one paragraph, heading or table cell.
//!
//! It follows CommonMark's inline rules, with its delimiter algorithm for
//! emphasis, and leaves out what a terminal cannot show or an answer has no
//! use for: inline HTML and strikethrough are shown as the text they are,
//! and reference links are not resolved.
//!
//! What it adds for streaming is the place of everything in the source:
//! every [`Piece`] knows the bytes it came from, and [`Parsed::opens`] says
//! where the first mark sits that text still to come could close (an
//! emphasis mark that can open, a backtick run with no closing run, a
//! bracket that is not a link's). A row of a paragraph before that place can
//! no longer change ([`super`], "The commit rule").

use std::ops::Range;

/// What a piece of text is drawn as, besides its colour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct Marks {
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) code: bool,
}

/// A run of shown text with one set of marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    /// What is shown. A tab is a space, a soft line break is a space, and a
    /// hard line break is `\n`.
    pub(crate) text: String,
    pub(crate) marks: Marks,
    /// The address, when the piece is a link's text.
    pub(crate) link: Option<String>,
    /// The bytes of the source it comes from.
    pub(crate) src: Range<usize>,
    /// Whether each byte of `text` comes from the byte of the source at the
    /// same distance from `src.start`, so that a place inside the piece maps
    /// back to the source.
    pub(crate) linear: bool,
    /// How many constructs (emphasis, code span, link) the piece is inside.
    pub(crate) depth: u16,
}

/// A parsed stretch of inline text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) pieces: Vec<Piece>,
    /// Every byte of the marks that text still to come could close, in
    /// order: emphasis marks that can open, backtick runs with no closing
    /// run, and brackets that are not a link's. Before the first, nothing
    /// can change; with all of them frozen, nothing can.
    pub(crate) opens: Vec<usize>,
}

impl Parsed {
    /// The first byte of the source that text still to come could change
    /// the rendering of, or `None`.
    pub(crate) fn open(&self) -> Option<usize> {
        self.opens.first().copied()
    }
}

/// Parses `source`. The bytes at the places in `frozen` are taken as text
/// even where they would open something: a row that was committed while a
/// mark in it was still open keeps that mark literal from then on.
pub(crate) fn parse(source: &str, frozen: &[usize]) -> Parsed {
    let mut parser = Parser {
        src: source,
        frozen,
        nodes: Vec::new(),
        brackets: Vec::new(),
    };
    parser.scan();
    let len = parser.nodes.len();
    process_emphasis(&mut parser.nodes, 0, len);
    finish(source, parser.nodes)
}

/// One node of the inline list before it is turned into pieces.
#[derive(Debug, Clone)]
enum Node {
    /// Shown text: the source bytes `src`, or `text` where it differs.
    Text {
        src: Range<usize>,
        text: Option<String>,
    },
    /// A hard line break.
    Break { src: Range<usize> },
    /// A code span: its content's bytes and what is shown of them.
    Code {
        content: Range<usize>,
        text: String,
    },
    /// A run of `*` or `_`; what is left of it is shown as text.
    Delim(Delim),
    /// A backtick run with no closing run: text a later run may close.
    OpenTicks { src: Range<usize> },
    /// A `[` or `![` that is not (or not yet) a link's.
    Bracket { src: Range<usize>, active: bool },
    /// The start of a link's text.
    LinkStart { url: String },
    /// The end of a link's text.
    LinkEnd,
}

#[derive(Debug, Clone)]
struct Delim {
    ch: u8,
    start: usize,
    len: usize,
    /// Characters taken from its start by the emphasis it closes, and from
    /// its end by the emphasis it opens.
    closed: usize,
    opened: usize,
    can_open: bool,
    can_close: bool,
    /// The emphasis it closes, innermost first, and opens, innermost first.
    closes: Vec<Kind>,
    opens: Vec<Kind>,
}

impl Delim {
    fn remaining(&self) -> usize {
        self.len - self.closed - self.opened
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Em,
    Strong,
}

struct Parser<'a> {
    src: &'a str,
    frozen: &'a [usize],
    nodes: Vec<Node>,
    /// Indexes of `Bracket` nodes still waiting for their `]`, innermost
    /// last.
    brackets: Vec<usize>,
}

impl Parser<'_> {
    fn is_frozen(&self, at: usize) -> bool {
        self.frozen.contains(&at)
    }

    fn scan(&mut self) {
        let bytes = self.src.as_bytes();
        let mut text_start = 0;
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            let special = matches!(
                byte,
                b'\\' | b'`' | b'*' | b'_' | b'[' | b'!' | b']' | b'<' | b'&' | b'\n'
            );
            if !special || (byte != b'\n' && self.is_frozen(at)) {
                at += 1;
                continue;
            }
            self.text(text_start, at);
            text_start = at;
            match self.special(at) {
                Some(next) => {
                    at = next;
                    text_start = next;
                }
                // Text after all: it starts the next stretch of text.
                None => at += 1,
            }
        }
        self.text(text_start, bytes.len());
    }

    fn text(&mut self, from: usize, to: usize) {
        if from < to {
            self.nodes.push(Node::Text {
                src: from..to,
                text: None,
            });
        }
    }

    /// Handles the special byte at `at`, pushing its nodes. Returns where
    /// scanning goes on, or `None` when the byte is text after all.
    fn special(&mut self, at: usize) -> Option<usize> {
        let bytes = self.src.as_bytes();
        match bytes[at] {
            b'\\' => {
                let next = *bytes.get(at + 1)?;
                if next == b'\n' {
                    self.nodes.push(Node::Break { src: at..at + 2 });
                    Some(at + 2)
                } else if next.is_ascii_punctuation() {
                    self.nodes.push(Node::Text {
                        src: at..at + 2,
                        text: Some(char::from(next).to_string()),
                    });
                    Some(at + 2)
                } else {
                    None
                }
            }
            b'\n' => Some(self.line_break(at)),
            b'`' => Some(self.backticks(at)),
            b'*' | b'_' => Some(self.delimiter_run(at)),
            b'[' => Some(self.bracket(at, at + 1)),
            b'!' if bytes.get(at + 1) == Some(&b'[') && !self.is_frozen(at + 1) => {
                Some(self.bracket(at, at + 2))
            }
            b']' => self.close_bracket(at),
            b'<' => self.autolink(at),
            b'&' => self.entity(at),
            _ => None,
        }
    }

    /// A line break in a paragraph: hard after two spaces, soft (shown as a
    /// space) otherwise. The spaces before it are never shown.
    fn line_break(&mut self, at: usize) -> usize {
        let spaces = self.src[..at].len() - self.src[..at].trim_end_matches(' ').len();
        if spaces > 0
            && let Some(Node::Text { src, text: None }) = self.nodes.last_mut()
            && src.end == at
        {
            let cut = at - spaces;
            if cut <= src.start {
                self.nodes.pop();
            } else {
                src.end = cut;
            }
        }
        let src = at - spaces..at + 1;
        if spaces >= 2 {
            self.nodes.push(Node::Break { src });
        } else {
            self.nodes.push(Node::Text {
                src,
                text: Some(" ".to_owned()),
            });
        }
        at + 1
    }

    /// The length of the run of `byte` starting at `at`, up to a frozen
    /// byte.
    fn run(&self, at: usize, byte: u8) -> usize {
        let bytes = self.src.as_bytes();
        let mut len = 0;
        while at + len < bytes.len() && bytes[at + len] == byte && !self.is_frozen(at + len) {
            len += 1;
        }
        len
    }

    fn backticks(&mut self, at: usize) -> usize {
        let bytes = self.src.as_bytes();
        let run = self.run(at, b'`');
        let content_start = at + run;
        // The next run of exactly the same length closes it.
        let mut search = content_start;
        while search < bytes.len() {
            if bytes[search] != b'`' {
                search += 1;
                continue;
            }
            let other = self.run(search, b'`');
            if other == 0 {
                // A frozen backtick: text.
                search += 1;
                continue;
            }
            if other == run {
                let raw = &self.src[content_start..search];
                let mut text = raw.replace('\n', " ");
                if text.len() >= 2
                    && text.starts_with(' ')
                    && text.ends_with(' ')
                    && !text.bytes().all(|b| b == b' ')
                {
                    text = text[1..text.len() - 1].to_owned();
                }
                self.nodes.push(Node::Code {
                    content: content_start..search,
                    text: text.replace('\t', " "),
                });
                return search + other;
            }
            search += other;
        }
        self.nodes.push(Node::OpenTicks {
            src: at..content_start,
        });
        content_start
    }

    fn delimiter_run(&mut self, at: usize) -> usize {
        let ch = self.src.as_bytes()[at];
        let len = self.run(at, ch);
        let before = self.src[..at].chars().next_back();
        let after = self.src[at + len..].chars().next();
        let space = |c: Option<char>| c.is_none_or(char::is_whitespace);
        let punct = |c: Option<char>| c.is_some_and(is_punctuation);
        let left = !space(after) && (!punct(after) || space(before) || punct(before));
        let right = !space(before) && (!punct(before) || space(after) || punct(after));
        let (can_open, can_close) = if ch == b'*' {
            (left, right)
        } else {
            (
                left && (!right || punct(before)),
                right && (!left || punct(after)),
            )
        };
        self.nodes.push(Node::Delim(Delim {
            ch,
            start: at,
            len,
            closed: 0,
            opened: 0,
            can_open,
            can_close,
            closes: Vec::new(),
            opens: Vec::new(),
        }));
        at + len
    }

    fn bracket(&mut self, at: usize, end: usize) -> usize {
        self.brackets.push(self.nodes.len());
        self.nodes.push(Node::Bracket {
            src: at..end,
            active: true,
        });
        end
    }

    fn close_bracket(&mut self, at: usize) -> Option<usize> {
        let opener = self.brackets.pop()?;
        let Node::Bracket { src: open_src, active } = self.nodes[opener].clone() else {
            return None;
        };
        if !active {
            return None;
        }
        // An inline link: `](destination "title")`.
        let (url, end) = link_tail(self.src, at + 1)?;
        if (at..end).any(|i| self.is_frozen(i)) {
            return None;
        }
        let len = self.nodes.len();
        process_emphasis(&mut self.nodes, opener + 1, len);
        // Marks inside the link that are left can no longer pair with marks
        // outside it.
        for node in &mut self.nodes[opener + 1..] {
            if let Node::Delim(delim) = node {
                delim.can_open = false;
                delim.can_close = false;
            }
        }
        let image = self.src.as_bytes()[open_src.start] == b'!';
        self.nodes[opener] = Node::LinkStart { url };
        self.nodes.push(Node::LinkEnd);
        // No links inside links: earlier brackets can no longer make one.
        if !image {
            for &earlier in &self.brackets {
                if let Node::Bracket { active, src } = &mut self.nodes[earlier]
                    && self.src.as_bytes()[src.start] == b'['
                {
                    *active = false;
                }
            }
        }
        Some(end)
    }

    fn autolink(&mut self, at: usize) -> Option<usize> {
        let rest = &self.src[at + 1..];
        let end = rest.find(|c: char| c == '>' || c == '<' || c.is_whitespace() || c.is_control())?;
        if rest.as_bytes()[end] != b'>' {
            return None;
        }
        let inner = &rest[..end];
        let scheme = &inner[..inner.find(':')?];
        let valid = (2..=32).contains(&scheme.len())
            && scheme.as_bytes()[0].is_ascii_alphabetic()
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'));
        let close = at + 1 + end + 1;
        if !valid || (at..close).any(|i| self.is_frozen(i)) {
            return None;
        }
        self.nodes.push(Node::LinkStart {
            url: inner.to_owned(),
        });
        self.nodes.push(Node::Text {
            src: at + 1..close - 1,
            text: None,
        });
        self.nodes.push(Node::LinkEnd);
        Some(close)
    }

    fn entity(&mut self, at: usize) -> Option<usize> {
        let rest = &self.src[at + 1..];
        let semi = rest.find(';').filter(|&semi| semi <= 32)?;
        let name = &rest[..semi];
        let ch = if let Some(number) = name.strip_prefix('#') {
            let value = match number.strip_prefix(['x', 'X']) {
                Some(hex) if (1..=6).contains(&hex.len()) => u32::from_str_radix(hex, 16).ok()?,
                None if (1..=7).contains(&number.len())
                    && number.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    number.parse().ok()?
                }
                _ => return None,
            };
            match char::from_u32(value) {
                Some('\0') | None => '\u{fffd}',
                Some(c) => c,
            }
        } else {
            named_entity(name)?
        };
        // A control character from an entity is shown as its symbol, as
        // any other from content is; a tab or a line feed is a space.
        let text = match ch {
            '\t' | '\n' => " ".to_owned(),
            _ => match cortex_term::sanitize::visible(ch) {
                Some(symbol) => symbol.to_string(),
                None => ch.to_string(),
            },
        };
        let end = at + 1 + semi + 1;
        self.nodes.push(Node::Text {
            src: at..end,
            text: Some(text),
        });
        Some(end)
    }
}

/// Turns the nodes into pieces, and finds the first place still open.
fn finish(src: &str, nodes: Vec<Node>) -> Parsed {
    let mut out = Out::default();
    // The constructs the walk is inside: emphasis kinds, and `None` for a
    // link.
    let mut stack: Vec<Option<Kind>> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    for node in nodes {
        match node {
            Node::Text { src: range, text } => {
                let (shown, linear) = match text {
                    Some(text) => {
                        let linear = text.len() == range.len();
                        (text, linear)
                    }
                    None => (src[range.clone()].replace('\t', " "), true),
                };
                out.push(shown, range, linear, false, &stack, &links);
            }
            Node::Break { src: range } => {
                out.push("\n".to_owned(), range, false, false, &stack, &links);
            }
            Node::Code { content, text } => {
                let raw = src[content.clone()].replace(['\n', '\t'], " ");
                let (range, linear) = if raw == text {
                    (content, true)
                } else if raw.len() == text.len() + 2 && raw[1..raw.len() - 1] == text {
                    (content.start + 1..content.end - 1, true)
                } else {
                    (content, false)
                };
                out.push(text, range, linear, true, &stack, &links);
            }
            Node::Delim(delim) => {
                for kind in &delim.closes {
                    if let Some(position) = stack.iter().rposition(|k| *k == Some(*kind)) {
                        stack.remove(position);
                    }
                }
                let shown = delim.remaining();
                if shown > 0 {
                    let from = delim.start + delim.closed;
                    if delim.can_open {
                        out.note_open(from..from + shown);
                    }
                    out.push(
                        src[from..from + shown].to_owned(),
                        from..from + shown,
                        true,
                        false,
                        &stack,
                        &links,
                    );
                }
                for kind in delim.opens.iter().rev() {
                    stack.push(Some(*kind));
                }
            }
            Node::OpenTicks { src: range } | Node::Bracket { src: range, .. } => {
                out.note_open(range.clone());
                out.push(src[range.clone()].to_owned(), range, true, false, &stack, &links);
            }
            Node::LinkStart { url } => {
                links.push(url);
                stack.push(None);
            }
            Node::LinkEnd => {
                links.pop();
                stack.pop();
            }
        }
    }
    let mut opens = out.opens;
    opens.sort_unstable();
    Parsed {
        pieces: out.pieces,
        opens,
    }
}

#[derive(Default)]
struct Out {
    pieces: Vec<Piece>,
    opens: Vec<usize>,
}

impl Out {
    fn note_open(&mut self, bytes: Range<usize>) {
        self.opens.extend(bytes);
    }

    fn push(
        &mut self,
        text: String,
        src: Range<usize>,
        linear: bool,
        code: bool,
        stack: &[Option<Kind>],
        links: &[String],
    ) {
        if text.is_empty() {
            return;
        }
        let marks = Marks {
            bold: stack.contains(&Some(Kind::Strong)),
            italic: stack.contains(&Some(Kind::Em)),
            code,
        };
        let depth = u16::try_from(stack.len() + usize::from(code)).unwrap_or(u16::MAX);
        let link = links.last().cloned();
        if let Some(last) = self.pieces.last_mut()
            && last.marks == marks
            && last.link == link
            && last.depth == depth
            && last.linear
            && linear
            && last.src.end == src.start
        {
            last.text.push_str(&text);
            last.src.end = src.end;
            return;
        }
        self.pieces.push(Piece {
            text,
            marks,
            link,
            src,
            linear,
            depth,
        });
    }
}

/// CommonMark's "process emphasis", over the nodes in `bottom..top`.
fn process_emphasis(nodes: &mut [Node], bottom: usize, top: usize) {
    let mut closer = bottom;
    while closer < top {
        let Node::Delim(c) = &nodes[closer] else {
            closer += 1;
            continue;
        };
        if !c.can_close || c.remaining() == 0 {
            closer += 1;
            continue;
        }
        let (ch, c_len, c_both) = (c.ch, c.len, c.can_open);
        // The nearest opener of the same character that may pair with it.
        let mut found = None;
        let mut index = closer;
        while index > bottom {
            index -= 1;
            if let Node::Delim(o) = &nodes[index]
                && o.ch == ch
                && o.can_open
                && o.remaining() > 0
            {
                let multiple_of_three = (o.can_close || c_both)
                    && (o.len + c_len) % 3 == 0
                    && !(o.len % 3 == 0 && c_len % 3 == 0);
                if !multiple_of_three {
                    found = Some(index);
                    break;
                }
            }
        }
        let Some(opener) = found else {
            closer += 1;
            continue;
        };
        let (o_left, c_left) = match (&nodes[opener], &nodes[closer]) {
            (Node::Delim(o), Node::Delim(c)) => (o.remaining(), c.remaining()),
            _ => (0, 0),
        };
        let (kind, used) = if o_left >= 2 && c_left >= 2 {
            (Kind::Strong, 2)
        } else {
            (Kind::Em, 1)
        };
        if let Node::Delim(o) = &mut nodes[opener] {
            o.opened += used;
            o.opens.push(kind);
        }
        if let Node::Delim(c) = &mut nodes[closer] {
            c.closed += used;
            c.closes.push(kind);
        }
        // Marks between them can no longer open or close anything.
        for node in &mut nodes[opener + 1..closer] {
            if let Node::Delim(d) = node {
                d.can_open = false;
                d.can_close = false;
            }
        }
        // The same closer may close more: it is looked at again.
    }
}

/// After a `]` at `at - 1`: `(destination "title")`. Returns the destination
/// and where the link ends.
fn link_tail(src: &str, at: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(at) != Some(&b'(') {
        return None;
    }
    let skip_space = |i: &mut usize| {
        while *i < bytes.len() && matches!(bytes[*i], b' ' | b'\t' | b'\n') {
            *i += 1;
        }
    };
    let mut i = at + 1;
    skip_space(&mut i);
    let url;
    if bytes.get(i) == Some(&b'<') {
        let close = src[i + 1..].find(['>', '\n', '<'])?;
        if bytes[i + 1 + close] != b'>' {
            return None;
        }
        url = src[i + 1..i + 1 + close].to_owned();
        i += close + 2;
    } else {
        let start = i;
        let mut depth = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if bytes.get(i + 1).is_some_and(u8::is_ascii_punctuation) => i += 2,
                b'(' => {
                    depth += 1;
                    i += 1;
                }
                b')' if depth == 0 => break,
                b')' => {
                    depth -= 1;
                    i += 1;
                }
                b if b.is_ascii_whitespace() || b.is_ascii_control() => break,
                _ => i += 1,
            }
        }
        url = src[start..i].to_owned();
    }
    skip_space(&mut i);
    if let Some(&quote @ (b'"' | b'\'' | b'(')) = bytes.get(i) {
        let close = if quote == b'(' { b')' } else { quote };
        let mut j = i + 1;
        while j < bytes.len() && bytes[j] != close {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        if j >= bytes.len() {
            return None;
        }
        i = j + 1;
        skip_space(&mut i);
    }
    (bytes.get(i) == Some(&b')')).then(|| (url, i + 1))
}

/// Punctuation as CommonMark's flanking rules read it: ASCII punctuation,
/// and, beyond ASCII, what is neither a letter or digit, nor white space,
/// nor a control character (an approximation of the Unicode categories P
/// and S).
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control())
}

/// The named entities an answer uses. Others are shown as written.
fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "middot" => '·',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "le" => '≤',
        "ge" => '≥',
        "ne" => '≠',
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
