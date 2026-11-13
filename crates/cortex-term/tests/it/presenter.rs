//! The inline presenter against the virtual terminal: for every frame, one
//! write, synchronized-update brackets, no clear followed by a redraw,
//! nothing above the live region touched; and the right screen after commits
//! and height changes (`docs/testing-and-ci.md`, "Testing the TUI").

use std::io;

use cortex_base::{Rng, SeededRng};
use cortex_term::{
    Capabilities, Color, Cursor, Drawn, Frame, Inline, Line, Overrides, Platform, Sink, Span, Style,
};
use cortex_testkit::VTerm;
use cortex_testkit::cases::{self, Outcome};

/// What a shell left on screen before the program started.
const SHELL: &[u8] = b"$ earlier output\r\nline two\r\n$ cortex\r\n";

/// The largest frame of a normal live region: one read of a
/// pseudo-terminal, so a frame cannot tear even without synchronized
/// updates (`docs/performance-budgets.md`).
const MAX_FRAME: usize = 4096;

/// Records each call the presenter makes.
#[derive(Debug, Default)]
struct Writes(Vec<Vec<u8>>);

impl Sink for Writes {
    fn write_frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

fn caps(sync: bool) -> Capabilities {
    let mut caps = Capabilities::detect(
        |name| (name == "TERM").then(|| "xterm-256color".to_owned()),
        Platform::Unix,
        Overrides::default(),
    );
    caps.sync = sync;
    caps
}

/// A terminal with a presenter on it, and the lines the screen should hold.
struct Rig {
    term: VTerm,
    presenter: Inline,
    caps: Capabilities,
    /// What the scrollback and screen should hold above the live region.
    expected: Vec<String>,
    frames: usize,
}

impl Rig {
    fn new(cols: u16, rows: u16, sync: bool) -> Self {
        let mut term = VTerm::new(cols, rows);
        term.feed(SHELL);
        let expected = term.history();
        Self {
            term,
            presenter: Inline::new(cols, rows),
            caps: caps(sync),
            expected,
            frames: 0,
        }
    }

    /// Draws one frame and checks every property of it. Returns the bytes,
    /// or nothing for a frame that was skipped.
    fn frame(
        &mut self,
        commit: &[Line],
        live: &[Line],
        cursor: Option<Cursor>,
    ) -> Result<Option<Vec<u8>>, String> {
        let live_top = self.term.cursor_line() - self.presenter.cursor_row();
        let mut writes = Writes::default();
        let drawn = self
            .presenter
            .draw(
                &Frame {
                    commit,
                    live,
                    cursor,
                },
                &self.caps,
                &mut writes,
            )
            .map_err(|error| error.to_string())?;
        self.frames += 1;
        let frame = self.frames;
        match (drawn, writes.0.len()) {
            (Drawn::Skipped, 0) => {
                if !commit.is_empty() {
                    return Err(format!("frame {frame}: skipped with lines to commit"));
                }
                return Ok(None);
            }
            (Drawn::Written(length), 1) if length == writes.0[0].len() => {}
            (drawn, count) => {
                return Err(format!(
                    "frame {frame}: {drawn:?} in {count} write(s), not one"
                ));
            }
        }
        let bytes = writes.0.remove(0);
        self.term.watch();
        self.term.feed(&bytes);
        let report = self.term.report();
        let problems = report.problems(live_top, self.caps.sync);
        if !problems.is_empty() {
            return Err(format!(
                "frame {frame}: {}\n  bytes: {:?}\n{}",
                problems.join("; "),
                String::from_utf8_lossy(&bytes),
                self.term.dump()
            ));
        }
        if !self.caps.sync && bytes.windows(8).any(|w| w == b"\x1b[?2026") {
            return Err(format!(
                "frame {frame}: brackets without synchronized updates"
            ));
        }
        let width = usize::from(self.presenter.width());
        for line in commit {
            self.expected.push(clip(&line.text(), width));
        }
        self.check_screen(live, cursor)?;
        Ok(Some(bytes))
    }

    /// The scrollback and screen hold what was committed, in order, then the
    /// live region; and the cursor is where the frame put it.
    fn check_screen(&self, live: &[Line], cursor: Option<Cursor>) -> Result<(), String> {
        let rows = self.term.size().1;
        let width = usize::from(self.presenter.width());
        let skip = live.len().saturating_sub(usize::from(rows));
        let mut want = self.expected.clone();
        want.extend(live[skip..].iter().map(|line| clip(&line.text(), width)));
        while want.last().is_some_and(String::is_empty) {
            want.pop();
        }
        let have = self.term.history();
        if have != want {
            return Err(format!(
                "frame {}: the screen is wrong\n  want: {want:?}\n  have: {have:?}\n{}",
                self.frames,
                self.term.dump()
            ));
        }
        if let Some(cursor) = cursor
            && cursor.row >= skip
        {
            let top = self.term.cursor_line() - self.presenter.cursor_row();
            let line = top + cursor.row - skip;
            let at = (self.term.cursor_line(), self.term.cursor().1);
            let col = cursor.col.min(width);
            if at != (line, col) || !self.term.modes().cursor_visible {
                return Err(format!(
                    "frame {}: the cursor is at {at:?}, visible {}, not at {:?}",
                    self.frames,
                    self.term.modes().cursor_visible,
                    (line, col)
                ));
            }
        } else if self.term.modes().cursor_visible {
            return Err(format!(
                "frame {}: the cursor is shown with none asked for",
                self.frames
            ));
        }
        Ok(())
    }
}

/// A line's text as the screen shows it: cut to `cols`, every control
/// character as its symbol, without trailing blanks.
fn clip(text: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for (_, grapheme, columns) in cortex_base::text::graphemes(text) {
        if width + columns > cols {
            break;
        }
        out.extend(
            grapheme
                .chars()
                .map(|c| cortex_term::sanitize::visible(c).unwrap_or(c)),
        );
        width += columns;
    }
    out.trim_end().to_owned()
}

/// One step of a generated script.
#[derive(Debug, Clone)]
enum Step {
    Frame {
        commit: Vec<String>,
        live: Vec<String>,
        cursor: Option<(usize, usize)>,
    },
    /// The terminal's height changes.
    Height(u16),
}

fn word(rng: &mut SeededRng) -> String {
    let words = [
        "a",
        "lorem",
        "streamed",
        "text",
        "x",
        "wide漢字",
        "",
        "end.",
    ];
    let mut line = String::new();
    for _ in 0..cases::small(rng, 12) {
        line.push_str(cases::pick(rng, &words));
        line.push(' ');
    }
    if rng.below(8) == 0 {
        // Exactly as wide as the screen, or wider.
        line = "#".repeat(40 + cases::small(rng, 3));
    }
    line.trim_end().to_owned()
}

fn script(rng: &mut SeededRng) -> (bool, Vec<Step>) {
    let sync = rng.below(4) != 0;
    let mut steps = Vec::new();
    for _ in 0..1 + cases::small(rng, 14) {
        if rng.below(10) == 0 {
            // Live regions stay within half of the smallest height.
            steps.push(Step::Height(10 + u16::try_from(rng.below(10)).unwrap_or(0)));
            continue;
        }
        let commit = (0..cases::small(rng, 6)).map(|_| word(rng)).collect();
        let live: Vec<String> = (0..cases::small(rng, 5)).map(|_| word(rng)).collect();
        let cursor = (!live.is_empty() && rng.below(4) != 0).then(|| {
            let row = usize::try_from(rng.below(live.len() as u64)).unwrap_or(0);
            (row, usize::try_from(rng.below(45)).unwrap_or(0))
        });
        steps.push(Step::Frame {
            commit,
            live,
            cursor,
        });
        if rng.below(3) == 0 {
            // The same frame again: drawn as nothing.
            let last = steps.last().cloned().unwrap();
            if let Step::Frame { live, cursor, .. } = last {
                steps.push(Step::Frame {
                    commit: Vec::new(),
                    live,
                    cursor,
                });
            }
        }
    }
    (sync, steps)
}

fn run_script((sync, steps): &(bool, Vec<Step>)) -> Outcome {
    let mut rig = Rig::new(40, 20, *sync);
    for step in steps {
        match step {
            Step::Frame {
                commit,
                live,
                cursor,
            } => {
                let commit: Vec<Line> = commit.iter().map(Line::plain).collect();
                let live: Vec<Line> = live.iter().map(Line::plain).collect();
                let cursor = cursor.map(|(row, col)| Cursor { row, col });
                rig.frame(&commit, &live, cursor)?;
            }
            Step::Height(rows) => {
                rig.term.resize(40, *rows);
                rig.presenter.resize(40, *rows);
                // What the terminal holds now is the truth about what was
                // committed: nothing is re-wrapped, nothing lost.
            }
        }
    }
    Ok(())
}

#[test]
fn every_frame_is_one_synchronized_write_that_clears_nothing_and_stays_in_the_live_region() {
    cases::check("presenter frames", 150, script, run_script);
}

#[test]
fn commits_go_into_the_scrollback_in_order_through_height_changes() {
    let mut rig = Rig::new(30, 8, true);
    let lines = |texts: &[&str]| -> Vec<Line> { texts.iter().map(|t| Line::plain(*t)).collect() };
    let cursor = |row| Some(Cursor { row, col: 2 });
    rig.frame(&[], &lines(&["status"]), cursor(0)).unwrap();
    rig.frame(
        &lines(&["one"]),
        &lines(&["tail", "> ", "status"]),
        cursor(1),
    )
    .unwrap();
    rig.frame(
        &lines(&["two", "three"]),
        &lines(&["> ", "status"]),
        cursor(0),
    )
    .unwrap();
    // The window gets shorter: rows above the cursor go into the scrollback.
    rig.term.resize(30, 4);
    rig.presenter.resize(30, 4);
    rig.frame(
        &lines(&["four", "five", "six"]),
        &lines(&["> hi", "status"]),
        cursor(0),
    )
    .unwrap();
    // And taller again.
    rig.term.resize(30, 12);
    rig.presenter.resize(30, 12);
    rig.frame(&[], &lines(&["a", "b", "> hi", "status"]), cursor(2))
        .unwrap();
    rig.frame(&lines(&["seven"]), &lines(&["> ", "status"]), cursor(0))
        .unwrap();
    let mut want: Vec<String> = [
        "$ earlier output",
        "line two",
        "$ cortex",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        ">",
        "status",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(rig.term.history(), want);
    // Finishing erases the live region and leaves the cursor where it began.
    let mut writes = Writes::default();
    rig.presenter.finish(&rig.caps, &mut writes).unwrap();
    assert_eq!(writes.0.len(), 1);
    rig.term.feed(&writes.0[0]);
    want.truncate(want.len() - 2);
    assert_eq!(rig.term.history(), want);
    assert_eq!(rig.term.cursor(), (rig.term.cursor().0, 0));
    assert!(rig.term.modes().cursor_visible && !rig.term.modes().sync);
}

#[test]
fn a_narrower_window_redraws_the_live_region_at_the_new_width() {
    let mut rig = Rig::new(30, 10, true);
    let live = |text: &str| vec![Line::plain(text), Line::plain("status line")];
    rig.frame(
        &[Line::plain("committed")],
        &live("a long editor line of text"),
        None,
    )
    .unwrap();
    rig.term.resize(12, 10);
    rig.presenter.resize(12, 10);
    rig.expected = rig.expected.iter().map(|line| clip(line, 12)).collect();
    rig.frame(&[], &live("a long editor line of text"), None)
        .unwrap();
    assert_eq!(rig.term.screen()[3..5], ["committed", "a long edit"]);
}

#[test]
fn a_streaming_frame_is_small() {
    // The demo's shape: one growing line, a rule, the editor and a status
    // line; a line committed now and then.
    let mut rig = Rig::new(120, 40, true);
    let mut sizes = Vec::new();
    let mut text = String::new();
    // 600 frames commit about 25 lines: enough for the sizes to settle, and
    // a fraction of a second in an unoptimised build.
    for token in 0..600u32 {
        text.push_str(if token % 10 == 9 { " marker" } else { " word" });
        let commit = if text.len() > 110 {
            vec![Line::plain(std::mem::take(&mut text))]
        } else {
            Vec::new()
        };
        let live = vec![
            Line::from(Span::new(
                text.clone(),
                Style::PLAIN.fg(Color::Rgb(200, 200, 255)),
            )),
            Line::styled("─".repeat(120), Style::PLAIN.dim()),
            Line::plain("> typed"),
            Line::styled(
                format!("status: streaming, {token} tokens"),
                Style::PLAIN.dim(),
            ),
        ];
        if let Some(bytes) = rig
            .frame(&commit, &live, Some(Cursor { row: 2, col: 7 }))
            .unwrap()
        {
            sizes.push(bytes.len());
        }
    }
    sizes.sort_unstable();
    let median = sizes[sizes.len() / 2];
    let max = *sizes.last().unwrap();
    assert!(median <= 1024, "median frame {median} bytes");
    assert!(max <= MAX_FRAME, "largest frame {max} bytes");
}

#[test]
fn content_cannot_write_an_escape_sequence() {
    let mut rig = Rig::new(60, 10, true);
    let hostile = [
        "\x1b[2J\x1b[H",
        "\x1b]0;stolen title\x07",
        "\x1b]8;;https://e.example\x1b\\link",
        "\u{9b}1;1H\u{9b}?25l",
        "\x1b[?2026l\x1b[?1049h",
        "\x1bP+q\x1b\\\x1b[6n\x1b[c",
        "a\rb\nc\x08\x7f\t",
    ];
    let lines: Vec<Line> = hostile
        .iter()
        .map(|text| {
            Line::plain(*text)
                .with(Span::new(*text, Style::PLAIN.bold()).link("https://ok.example"))
        })
        .collect();
    rig.frame(&lines[..4], &lines[4..], Some(Cursor { row: 1, col: 3 }))
        .unwrap();
    assert_eq!(rig.term.title(), "");
    assert!(rig.term.take_queries().is_empty());
    let modes = rig.term.modes();
    assert!(!modes.alternate && !modes.sync && modes.cursor_visible);
    // Every control character shows as a symbol.
    let shown = rig.term.history().join("\n");
    assert!(shown.contains("\u{241b}[2J\u{241b}[H"), "{shown}");
    assert!(
        shown.contains("a\u{240d}b\u{240a}c\u{2408}\u{2421}\u{2409}"),
        "{shown}"
    );
}
