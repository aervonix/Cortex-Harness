//! `cargo xtask perf`: the demo measured against the TUI's budgets, by the
//! method of `docs/measurements/tui-language-spike.md`.
//!
//! The demo runs in a pseudo-terminal of 120 by 40 (ConPTY on Windows),
//! which answers its queries as a modern terminal would. The test records
//! every piece of output with the time it arrived and parses it.
//!
//! | Measure | How |
//! |---|---|
//! | Start to first frame | From starting the process to the piece after which the status line shows. 30 runs. |
//! | Key latency, idle | From writing a key to the piece after which the editor shows it, one key at a time. |
//! | Key latency, streaming | The same while 2,000 tokens a second stream, a key every 137 ms. |
//! | The demo's own share of it | From reading the key to having written the frame that shows it, measured inside the demo. The rest is the terminal's: the pseudo-terminal's or ConPTY's. |
//! | Frame cost | Building and writing a frame, measured inside the demo; a normal live region and a 30-line one. |
//! | Frames | Delimited by the synchronized-update brackets in the output. A drop is a gap between frames longer than 1.5 frame intervals while streaming. |
//! | Bytes per frame, reads per frame | From the recording. |
//! | Flicker | Each frame replayed through the virtual terminal: rows cleared and drawn again, frames outside brackets. |
//!
//! It is ignored in every ordinary run. `cargo xtask perf` builds the demo
//! with optimisation and runs it; `CORTEX_PERF_DEMO` names the demo binary
//! (the test binary itself is the demo when it is not set),
//! `CORTEX_PERF_RUNS` the start-up runs, `CORTEX_PERF_SECONDS` how long each
//! stream lasts, and `CORTEX_PERF_JSON` a file for the results.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use cortex_testkit::VTerm;
use cortex_testkit::pty::Session;

use super::demo::STATUS_MARK;
use super::{DEMO, child_arguments, this_binary};

const COLS: u16 = 120;
const ROWS: u16 = 40;
const LIMIT: Duration = Duration::from_secs(30);
/// The frame interval at 60 a second.
const FRAME: Duration = Duration::from_micros(16_667);
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The program and the arguments that run the demo with `options`.
fn demo(options: &[&str]) -> (String, Vec<String>) {
    match std::env::var("CORTEX_PERF_DEMO") {
        Ok(program) if !program.is_empty() => {
            (program, options.iter().map(|o| (*o).to_owned()).collect())
        }
        _ => (this_binary(), child_arguments(DEMO, &options.join(","))),
    }
}

fn start(options: &[&str]) -> Session {
    let (program, arguments) = demo(options);
    Session::start(&program, &arguments, COLS, ROWS).answering()
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The value at percentile `p` (0 to 100) of sorted values.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (p / 100.0 * (sorted.len() - 1) as f64).round();
    sorted[(rank as usize).min(sorted.len() - 1)]
}

fn sorted(mut values: Vec<f64>) -> Vec<f64> {
    values.sort_by(f64::total_cmp);
    values
}

/// The editor's text, from the row the demo draws it on.
fn editor(term: &VTerm) -> Option<String> {
    term.screen().into_iter().find_map(|row| {
        row.strip_prefix("> ")
            .map(str::to_owned)
            .or_else(|| (row == ">").then(String::new))
    })
}

fn quit(session: &mut Session) {
    session.send(b"\x03");
    session.wait_exit(LIMIT);
}

/// Types keys one at a time, each when the last has shown, and returns the
/// latencies in milliseconds.
fn idle_keys(count: usize) -> Vec<f64> {
    let mut session = start(&["rate=400"]);
    session.wait_for(LIMIT, "the first frame", |t| t.shows(STATUS_MARK));
    let mut typed = String::new();
    let mut latencies = Vec::new();
    for index in 0..count {
        let c = char::from(b'a' + u8::try_from(index % 26).unwrap_or(0));
        typed.push(c);
        let sent = session.send(c.to_string().as_bytes());
        let shown = session.wait_for(LIMIT, "a key's echo", |t| {
            editor(t).as_deref() == Some(typed.as_str())
        });
        latencies.push(millis(shown.saturating_duration_since(sent)));
    }
    quit(&mut session);
    latencies
}

/// What one streamed run gave.
struct Stream {
    keys: Vec<f64>,
    keys_lost: usize,
    demo_frames: Vec<(f64, usize)>,
    demo_keys: Vec<f64>,
    frames: Frames,
}

/// Streams for `seconds` with `options`, typing a key every 137 ms.
fn stream(options: &[&str], seconds: u64, stats: &std::path::Path) -> Stream {
    let turn = format!("turn-ms={}", seconds * 1000);
    let stats_option = format!("stats={}", stats.display());
    let mut all: Vec<&str> = vec!["rate=2000", "auto", "quit-after-turn", &turn, &stats_option];
    all.extend_from_slice(options);
    let mut session = start(&all);
    session.wait_for(LIMIT, "the first frame", |t| t.shows(STATUS_MARK));
    let every = Duration::from_millis(137);
    let mut next = Instant::now() + every;
    let mut typed = String::new();
    let mut sent: Vec<Instant> = Vec::new();
    let mut shown: Vec<Option<Instant>> = Vec::new();
    let end = Instant::now() + Duration::from_secs(seconds);
    while !session.ended() && Instant::now() < end + Duration::from_secs(5) {
        let now = Instant::now();
        if now >= next && now < end && typed.len() < usize::from(COLS) - 4 {
            let c = char::from(b'a' + u8::try_from(typed.len() % 26).unwrap_or(0));
            typed.push(c);
            sent.push(session.send(c.to_string().as_bytes()));
            shown.push(None);
            next += every;
        }
        let wait = next
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        if let Some(at) = session.step(wait)
            && let Some(text) = editor(session.screen())
        {
            {
                for (index, slot) in shown.iter_mut().enumerate() {
                    if slot.is_none() && text.len() > index {
                        *slot = Some(at);
                    }
                }
            }
        }
    }
    session.wait_exit(LIMIT);
    let keys: Vec<f64> = sent
        .iter()
        .zip(&shown)
        .filter_map(|(sent, shown)| {
            shown.map(|shown| millis(shown.saturating_duration_since(*sent)))
        })
        .collect();
    let keys_lost = shown.iter().filter(|s| s.is_none()).count();
    let (demo_frames, demo_keys) = read_stats(stats);
    let frames = frames(session.recording());
    Stream {
        keys,
        keys_lost,
        demo_frames,
        demo_keys,
        frames,
    }
}

/// The demo's own timings: frames (milliseconds, bytes) and keys
/// (milliseconds).
#[expect(
    clippy::disallowed_methods,
    reason = "a measurement reads the demo's file"
)]
fn read_stats(path: &std::path::Path) -> (Vec<(f64, usize)>, Vec<f64>) {
    let text = std::fs::read_to_string(path).expect("the demo's statistics");
    let numbers = |key: &str| -> Vec<u64> {
        let start = text.find(&format!("\"{key}\":[")).expect("a list") + key.len() + 4;
        let end = start + find_end(&text[start..]);
        text[start..end]
            .split(|c: char| !c.is_ascii_digit())
            .filter(|n| !n.is_empty())
            .map(|n| n.parse().expect("a number"))
            .collect()
    };
    let frames = numbers("frames")
        .chunks(2)
        .map(|pair| {
            (
                pair[0] as f64 / 1000.0,
                usize::try_from(pair[1]).unwrap_or(0),
            )
        })
        .collect();
    let keys = numbers("keys")
        .iter()
        .map(|&micros| micros as f64 / 1000.0)
        .collect();
    (frames, keys)
}

/// Where the list that starts the text ends.
fn find_end(text: &str) -> usize {
    let mut depth = 1;
    for (index, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// What the recording says about frames.
#[derive(Debug, Default)]
struct Frames {
    count: usize,
    bytes: Vec<f64>,
    /// Reads each frame spanned.
    reads: Vec<usize>,
    /// Gaps between frames, in milliseconds, while streaming.
    intervals: Vec<f64>,
    dropped: usize,
    cleared_then_drawn: usize,
    outside_brackets: usize,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| at + from)
}

fn frames(recording: &[cortex_testkit::pty::Chunk]) -> Frames {
    let mut all = Vec::new();
    let mut ends = Vec::new();
    for chunk in recording {
        all.extend_from_slice(&chunk.bytes);
        ends.push((all.len(), chunk.at));
    }
    let chunk_of = |offset: usize| ends.partition_point(|(end, _)| *end <= offset);
    let mut result = Frames::default();
    let mut term = VTerm::new(COLS, ROWS);
    let mut fed = 0;
    let mut times = Vec::new();
    let mut from = 0;
    while let Some(begin) = find(&all, SYNC_BEGIN, from) {
        let Some(end) = find(&all, SYNC_END, begin) else {
            break;
        };
        let end = end + SYNC_END.len();
        term.feed(&all[fed..begin]);
        term.watch();
        term.feed(&all[begin..end]);
        let report = term.report();
        if !report.cleared_then_drawn.is_empty() {
            result.cleared_then_drawn += 1;
        }
        fed = end;
        result.count += 1;
        result.bytes.push((end - begin) as f64);
        let (first, last) = (chunk_of(begin), chunk_of(end - 1));
        result.reads.push(last - first + 1);
        times.push(ends[last.min(ends.len() - 1)].1);
        from = end;
    }
    // Output outside any bracket that draws something: what a frame without
    // brackets would look like.
    result.outside_brackets = usize::from(result.count == 0 && !all.is_empty());
    // Streaming is the stretch of frames close together.
    for pair in times.windows(2) {
        let gap = pair[1] - pair[0];
        if gap < Duration::from_millis(500) {
            result.intervals.push(millis(gap));
            if gap > FRAME * 3 / 2 {
                result.dropped += 1;
            }
        }
    }
    result
}

fn row(out: &mut String, name: &str, values: &[f64], budget: &str) {
    let values = sorted(values.to_vec());
    let _ = writeln!(
        out,
        "| {name} | {:.2} | {:.2} | {:.2} | {} | {budget} |",
        percentile(&values, 50.0),
        percentile(&values, 95.0),
        percentile(&values, 99.0),
        values.len(),
    );
}

#[test]
#[ignore = "a measurement, run by `cargo xtask perf`"]
fn perf_the_demo_against_the_budgets() {
    let runs = env_number("CORTEX_PERF_RUNS", 30);
    let seconds = env_number("CORTEX_PERF_SECONDS", 12);
    let scratch = std::env::temp_dir().join(format!("cortex-perf-{}", std::process::id()));
    #[expect(
        clippy::disallowed_methods,
        reason = "a measurement makes its own directory"
    )]
    std::fs::create_dir_all(&scratch).expect("a directory for the demo's statistics");

    let mut first_frames = Vec::new();
    for _ in 0..runs {
        let mut session = start(&["rate=400"]);
        let drawn = session.wait_for(LIMIT, "the first frame", |t| t.shows(STATUS_MARK));
        first_frames.push(millis(drawn - session.started()));
        quit(&mut session);
    }
    let idle = idle_keys(30);
    let light = stream(&[], seconds, &scratch.join("light.json"));
    let heavy = stream(&["live-rows=30"], seconds, &scratch.join("heavy.json"));

    let mut out = String::new();
    let _ = writeln!(
        out,
        "\n## The demo against the budgets ({})\n",
        std::env::consts::OS
    );
    let _ = writeln!(
        out,
        "| Measure (ms) | median | 95th | 99th | samples | budget |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|");
    row(
        &mut out,
        "Start to first frame",
        &first_frames,
        "desktop 20, runner 50 / 120",
    );
    row(&mut out, "Key to frame, idle", &idle, "99th 2");
    row(
        &mut out,
        "Key to frame, streaming 2,000 tok/s",
        &light.keys,
        "99th 4",
    );
    row(
        &mut out,
        "  of which the demo's (read to written)",
        &light.demo_keys,
        "",
    );
    let cost = |s: &Stream| s.demo_frames.iter().map(|(ms, _)| *ms).collect::<Vec<_>>();
    row(
        &mut out,
        "Frame cost, normal live region",
        &cost(&light),
        "99th 1",
    );
    row(
        &mut out,
        "Frame cost, 30-line live region",
        &cost(&heavy),
        "99th 2",
    );
    row(
        &mut out,
        "Frame interval while streaming",
        &light.frames.intervals,
        "99th 18",
    );
    row(
        &mut out,
        "Bytes per frame, normal",
        &light.frames.bytes,
        "median 1024, max 4096",
    );
    row(
        &mut out,
        "Bytes per frame, 30 lines",
        &heavy.frames.bytes,
        "",
    );
    let max_bytes = light.frames.bytes.iter().copied().fold(0.0, f64::max);
    let _ = writeln!(out);
    for (name, s) in [("normal", &light), ("30 lines", &heavy)] {
        let split = s.frames.reads.iter().filter(|&&r| r > 1).count();
        let _ = writeln!(
            out,
            "- {name}: {} frames in brackets, {} dropped ({:.2}%), {} split across reads, {} with a row cleared and drawn again, {} keys lost{}",
            s.frames.count,
            s.frames.dropped,
            100.0 * s.frames.dropped as f64 / s.frames.intervals.len().max(1) as f64,
            split,
            s.frames.cleared_then_drawn,
            s.keys_lost,
            if s.frames.outside_brackets > 0 {
                "; the terminal passed on no brackets (ConPTY re-renders)"
            } else {
                ""
            },
        );
    }
    let _ = writeln!(out, "- largest normal frame: {max_bytes} bytes");
    println!("{out}");
    if let Ok(path) = std::env::var("CORTEX_PERF_JSON") {
        let json = format!(
            "{{\"os\":\"{}\",\"first_frame_ms\":{:?},\"idle_key_ms\":{:?},\"streaming_key_ms\":{:?},\"demo_key_ms\":{:?},\"frame_cost_ms\":{:?},\"heavy_frame_cost_ms\":{:?},\"frame_interval_ms\":{:?},\"bytes\":{:?}}}\n",
            std::env::consts::OS,
            sorted(first_frames),
            sorted(idle),
            sorted(light.keys.clone()),
            sorted(light.demo_keys.clone()),
            sorted(cost(&light)),
            sorted(cost(&heavy)),
            sorted(light.frames.intervals.clone()),
            sorted(light.frames.bytes.clone()),
        );
        #[expect(
            clippy::disallowed_methods,
            reason = "a measurement writes its results"
        )]
        std::fs::write(path, json).expect("write the results");
    }
}
