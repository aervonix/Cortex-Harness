//! Tests of the output policy: its three promises as properties over
//! generated output, and each way of keeping lines by example.

use cortex_base::{Rng, SeededRng};
use cortex_protocol::ids::ArtifactId;
use cortex_protocol::item::ToolOutcome;
use cortex_protocol::message::ImagePart;

use super::*;
use crate::cases::{self, Outcome, ensure};
use crate::store::{MemoryStore, Unavailable};

/// Limits small enough that generated output is cut by each of them in
/// turn: by lines, by bytes, and by line length.
fn small_limits(rng: &mut SeededRng) -> OutputLimits {
    match rng.below(3) {
        0 => OutputLimits::new(1 << 20, 8 + rng.below(12) as usize, 2000),
        1 => OutputLimits::new(4096, 10_000, 2000),
        _ => OutputLimits::new(
            4096 + rng.below(4096) as usize,
            8 + rng.below(40) as usize,
            256,
        ),
    }
}

fn any_keep(rng: &mut SeededRng) -> Keep {
    *cases::pick(rng, &[Keep::Head, Keep::HeadAndTail, Keep::Important])
}

/// One line of output: usually short, sometimes blank, sometimes long,
/// sometimes with characters that take several bytes or several code points.
fn line(rng: &mut SeededRng) -> String {
    const WORDS: &[&str] = &[
        "ok",
        "error:",
        "test",
        "warning",
        "Compiling",
        "passed",
        "naïve",
        "日本語",
        "👩‍💻",
        "e\u{301}",
        "[",
        "…",
        "42",
        "0.1",
        "src/main.rs:7",
        "",
        " ",
    ];
    let words = match rng.below(12) {
        0 => 0,
        1 => 60 + rng.below(120) as usize,
        _ => 1 + cases::small(rng, 12),
    };
    let mut line = String::new();
    for index in 0..words {
        if index > 0 {
            line.push(' ');
        }
        line.push_str(cases::pick(rng, WORDS));
    }
    line
}

/// Output as a tool might produce it, with or without a final line break,
/// and with runs of repeated lines.
fn output(rng: &mut SeededRng) -> String {
    let count = match rng.below(4) {
        0 => cases::small(rng, 6),
        _ => 5 + rng.below(60) as usize,
    };
    let mut lines = Vec::new();
    while lines.len() < count {
        let next = line(rng);
        let copies = if rng.below(10) == 0 {
            2 + rng.below(6) as usize
        } else {
            1
        };
        for _ in 0..copies {
            lines.push(next.clone());
        }
    }
    let mut text = lines.join("\n");
    if rng.below(2) == 0 {
        text.push('\n');
    }
    text
}

/// The lines the policy sees in `text` when nothing is compressed.
fn lines_of(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .collect()
}

/// Checks that `shaped` is an exact account of `source`: every line of its
/// text is a whole line of the source, a line shortened and marked as such,
/// or a marker for exactly the lines left out at that place.
fn account_is_exact(source: &[&str], shaped: &Shaped, limits: &OutputLimits) -> Outcome {
    let shown: Vec<&str> = if shaped.pieces.is_empty() {
        Vec::new()
    } else {
        shaped.text.split('\n').collect()
    };
    ensure!(
        shown.len() == shaped.pieces.len(),
        "{} lines of text for {} pieces",
        shown.len(),
        shaped.pieces.len()
    );
    ensure!(shaped.total_lines == source.len(), "the total is wrong");

    let mut next = 0;
    let mut omitted = 0;
    let mut clipped = 0;
    for (text, piece) in shown.iter().zip(&shaped.pieces) {
        match piece {
            Piece::Line {
                source: index,
                clipped: false,
            } => {
                ensure!(*index == next, "line {index} is out of order, after {next}");
                ensure!(
                    *text == source[*index],
                    "line {index} was changed: {text:?}"
                );
                next += 1;
            }
            Piece::Line {
                source: index,
                clipped: true,
            } => {
                ensure!(*index == next, "line {index} is out of order, after {next}");
                let whole = source[*index];
                let Some((kept, marker)) = text.rsplit_once("… [+") else {
                    return Err(format!("a shortened line has no marker: {text:?}").into());
                };
                ensure!(
                    whole.starts_with(kept),
                    "the kept part is not how the line began"
                );
                let missing = whole.chars().count() - kept.chars().count();
                ensure!(
                    marker == format!("{} characters]", count(missing)),
                    "the marker {marker:?} does not say {missing} characters are missing"
                );
                ensure!(missing > 0, "a line was marked and not shortened");
                // The cut is between grapheme clusters of the whole line.
                ensure!(
                    graphemes(whole).any(|(offset, _, _)| offset == kept.len()),
                    "the cut at byte {} splits a grapheme cluster",
                    kept.len()
                );
                ensure!(
                    text.chars().count() <= limits.max_line_chars(),
                    "a shortened line is still over the limit"
                );
                clipped += 1;
                next += 1;
            }
            Piece::Gap { lines } => {
                ensure!(*lines > 0, "a marker for no lines");
                ensure!(
                    *text == gap_marker(*lines),
                    "the marker {text:?} is not for {lines} lines"
                );
                next += lines;
                omitted += lines;
            }
        }
    }
    ensure!(
        next == source.len(),
        "the pieces cover {next} of {} lines",
        source.len()
    );
    ensure!(
        omitted == shaped.omitted_lines,
        "the count of omitted lines is wrong"
    );
    ensure!(
        clipped == shaped.clipped_lines,
        "the count of shortened lines is wrong"
    );
    // Two markers never stand side by side: each says all of its gap.
    for pair in shaped.pieces.windows(2) {
        ensure!(
            !matches!(pair, [Piece::Gap { .. }, Piece::Gap { .. }]),
            "two markers in a row"
        );
    }
    Ok(())
}

#[test]
fn it_never_cuts_inside_a_line() {
    cases::check(
        "output_whole_lines",
        200,
        |rng| (output(rng), small_limits(rng), any_keep(rng)),
        |(text, limits, keep)| {
            let shaped = shape(text, limits, *keep, Compression::None);
            account_is_exact(&lines_of(text), &shaped, limits)
        },
    );
}

#[test]
fn it_always_says_what_was_cut() {
    cases::check(
        "output_says_what_was_cut",
        200,
        |rng| (output(rng), small_limits(rng), any_keep(rng)),
        |(text, limits, keep)| {
            let shaped = shape(text, limits, *keep, Compression::None);
            let source = lines_of(text);
            let artifact = ArtifactRef {
                id: ArtifactId::new("a_0123456789abcdefghijkl"),
                label: "full output".to_owned(),
                bytes: None,
            };
            let notice = shaped.notice(text.len(), Ok(&artifact));
            if shaped.is_lossy() {
                let Some(notice) = notice else {
                    return Err("output was cut and there is no notice".into());
                };
                ensure!(
                    notice.contains(artifact.id.as_str()),
                    "the notice does not name the artifact"
                );
                if shaped.omitted_lines > 0 {
                    let said = format!(
                        "{} of {} lines not shown",
                        count(shaped.omitted_lines),
                        count(source.len())
                    );
                    ensure!(
                        notice.contains(&said),
                        "the notice does not say {said:?}: {notice}"
                    );
                }
                if shaped.clipped_lines > 0 {
                    let said = format!("{} long lines shortened", count(shaped.clipped_lines));
                    ensure!(
                        notice.contains(&said),
                        "the notice does not say {said:?}: {notice}"
                    );
                }
            } else {
                ensure!(notice.is_none(), "a notice for output that lost nothing");
                ensure!(
                    shaped.text == source.join("\n"),
                    "output that fits was changed"
                );
            }
            Ok(())
        },
    );
}

#[test]
fn what_it_returns_is_within_the_limits() {
    cases::check(
        "output_within_limits",
        200,
        |rng| {
            let compression = *cases::pick(rng, &[Compression::None, Compression::Structural]);
            (output(rng), small_limits(rng), any_keep(rng), compression)
        },
        |(text, limits, keep, compression)| {
            let shaped = shape(text, limits, *keep, *compression);
            let artifact = ArtifactRef {
                id: ArtifactId::new("a_0123456789abcdefghijkl"),
                label: "full output".to_owned(),
                bytes: None,
            };
            let mut whole = shaped.text.clone();
            if let Some(notice) = shaped.notice(text.len(), Ok(&artifact)) {
                ensure!(
                    notice.len() <= NOTICE_BYTES,
                    "the notice has {} bytes",
                    notice.len()
                );
                ensure!(!notice.contains('\n'), "the notice is more than one line");
                whole.push('\n');
                whole.push_str(&notice);
            }
            ensure!(
                limits.fits(&whole),
                "{} bytes and {} lines do not fit {limits:?}",
                whole.len(),
                whole.split('\n').count()
            );
            Ok(())
        },
    );
}

#[test]
fn the_artifact_holds_everything() {
    cases::check(
        "output_artifact_whole",
        200,
        |rng| (output(rng), small_limits(rng)),
        |(text, limits)| {
            let store = MemoryStore::new();
            let result = enforce(
                ToolResult::success(text.clone()),
                "demo",
                limits,
                None,
                &store,
            );
            ensure!(result.is_bounded(), "the result is not marked as passed");
            ensure!(
                limits.fits(&result.text()),
                "the result does not fit the limits"
            );
            match result.artifacts() {
                [] => {
                    ensure!(store.artifact_count() == 0, "an artifact nobody refers to");
                    // Nothing was lost: every line of the output is there,
                    // or is counted by a marker for repeats.
                    let shaped = shape(text, limits, Keep::HeadAndTail, Compression::Structural);
                    ensure!(
                        !shaped.is_lossy(),
                        "output was cut and no artifact was kept"
                    );
                }
                [artifact] => {
                    let kept = store.artifact(&artifact.id);
                    ensure!(
                        kept.as_deref() == Some(text.as_bytes()),
                        "the artifact is not the output as it was produced"
                    );
                    ensure!(
                        artifact.bytes == Some(text.len() as u64),
                        "the size is wrong"
                    );
                    ensure!(
                        result.text().contains(artifact.id.as_str()),
                        "the result does not name its artifact"
                    );
                    ensure!(store.artifact_count() == 1, "more than one artifact");
                }
                more => return Err(format!("{} artifacts for one result", more.len()).into()),
            }
            // Passing it again changes nothing, and stores nothing.
            let again = enforce(result.clone(), "demo", limits, None, &store);
            ensure!(again == result, "a second pass changed the result");
            Ok(())
        },
    );
}

#[test]
fn compressed_output_has_no_control_characters_and_is_still_accounted_for() {
    cases::check(
        "output_compressed",
        150,
        |rng| {
            // The same output, with colour codes and redraws put in.
            let plain = output(rng);
            let mut noisy = String::new();
            for (index, line) in lines_of(&plain).into_iter().enumerate() {
                if index > 0 {
                    noisy.push('\n');
                }
                match rng.below(5) {
                    0 => {
                        noisy.push_str("\u{1b}[32m");
                        noisy.push_str(line);
                        noisy.push_str("\u{1b}[0m");
                    }
                    1 if !line.is_empty() => {
                        noisy.push_str("working\r");
                        noisy.push_str(line);
                    }
                    2 => {
                        noisy.push_str(line);
                        noisy.push('\r');
                    }
                    _ => noisy.push_str(line),
                }
            }
            if plain.ends_with('\n') {
                noisy.push('\n');
            }
            (plain, noisy, small_limits(rng), any_keep(rng))
        },
        |(plain, noisy, limits, keep)| {
            let shaped = shape(noisy, limits, *keep, Compression::Structural);
            ensure!(
                !shaped
                    .text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t'),
                "a control character survived"
            );
            // The noise changes nothing: the plain output shapes the same.
            let expected = shape(plain, limits, *keep, Compression::Structural);
            ensure!(
                shaped == expected,
                "colour codes and redraws changed the outcome"
            );
            Ok(())
        },
    );
}

fn numbered(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

/// Numbered lines whose words differ from one line to the next, so that
/// they are not similar lines to fold.
fn varied(count: usize) -> String {
    (1..=count)
        .map(|n| format!("{} {n}\n", ["alpha", "beta", "gamma"][n % 3]))
        .collect()
}

#[test]
fn output_that_fits_is_returned_as_it_is() {
    let limits = OutputLimits::default();
    for text in [
        "",
        "one line",
        "two\nlines\n",
        "\n",
        "\n\n",
        "trailing space \n",
    ] {
        let shaped = shape(text, &limits, Keep::Head, Compression::None);
        assert_eq!(
            shaped.text,
            text.strip_suffix('\n').unwrap_or(text),
            "{text:?}"
        );
        assert!(!shaped.is_lossy());
        assert_eq!(shaped.notice(text.len(), Err("unused")), None);
    }
    assert_eq!(
        shape("", &limits, Keep::Head, Compression::None).total_lines,
        0
    );
    assert_eq!(
        shape("\n", &limits, Keep::Head, Compression::None).total_lines,
        1
    );
    assert_eq!(
        shape("a\n\n", &limits, Keep::Head, Compression::None).total_lines,
        2
    );
}

#[test]
fn the_head_is_kept_for_content_read_from_its_start() {
    let limits = OutputLimits::new(1 << 20, 10, 2000);
    let shaped = shape(&numbered(50), &limits, Keep::Head, Compression::None);
    // Ten lines, less the notice and the marker.
    assert_eq!(
        shaped.text,
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n\
         [… 42 lines not shown …]"
    );
    assert_eq!(shaped.omitted_lines, 42);
    assert_eq!(shaped.total_lines, 50);
}

#[test]
fn the_head_and_the_tail_are_kept_for_command_output() {
    let limits = OutputLimits::new(1 << 20, 14, 2000);
    let shaped = shape(
        &numbered(100),
        &limits,
        Keep::HeadAndTail,
        Compression::None,
    );
    let lines: Vec<&str> = shaped.text.split('\n').collect();
    assert_eq!(lines.first(), Some(&"line 1"));
    assert_eq!(lines.last(), Some(&"line 100"));
    assert_eq!(lines.len(), 13, "fourteen lines less the notice");
    let head = lines
        .iter()
        .take_while(|line| !line.starts_with('['))
        .count();
    let tail = lines.len() - head - 1;
    assert_eq!(
        (head, tail),
        (3, 9),
        "a quarter and three quarters: {lines:?}"
    );
    assert_eq!(lines[head], "[… 88 lines not shown …]");
}

#[test]
fn problem_lines_come_first_for_builds_and_tests() {
    let mut text = String::new();
    for n in 1..=200 {
        if n == 120 {
            text.push_str("error[E0308]: mismatched types\n  --> src/lib.rs:7:5\n   |\n");
        } else {
            text.push_str(&format!("   Compiling crate-{n} v0.1.0\n"));
        }
    }
    let limits = OutputLimits::new(1 << 20, 30, 2000);
    let shaped = shape(&text, &limits, Keep::Important, Compression::None);
    assert!(
        shaped.text.contains(
            "Compiling crate-119 v0.1.0\nerror[E0308]: mismatched types\n  --> src/lib.rs:7:5\n   |"
        ),
        "{}",
        shaped.text
    );
    assert!(shaped.text.starts_with("   Compiling crate-1 v0.1.0\n"));
    assert!(shaped.text.ends_with("   Compiling crate-200 v0.1.0"));
    assert!(shaped.omitted_lines > 150);

    // With the head kept, the error is among the lines left out.
    let head = shape(&text, &limits, Keep::Head, Compression::None);
    assert!(!head.text.contains("error[E0308]"));
}

#[test]
fn a_long_line_is_shortened_between_characters_and_says_by_how_much() {
    let limits = OutputLimits::new(1 << 20, 100, 300);
    let long = "é".repeat(1000);
    let clipped = limits.clip(&long);
    let length = clipped.chars().count();
    assert!(
        (290..=300).contains(&length),
        "{length}: the marker is inside the limit"
    );
    let (kept, marker) = clipped.rsplit_once("… [+").expect("a marker");
    assert_eq!(
        marker,
        format!("{} characters]", 1000 - kept.chars().count())
    );

    assert!(matches!(limits.clip("short"), Cow::Borrowed("short")));
    // Exactly at the limit: not shortened.
    let exact = "x".repeat(300);
    assert_eq!(limits.clip(&exact), exact);

    // A cluster of several code points is kept whole or left out whole.
    let family = "👨‍👩‍👧‍👦".repeat(200);
    let clipped = limits.clip(&family);
    let (kept, _) = clipped.rsplit_once("… [+").expect("a marker");
    assert_eq!(kept.chars().count() % "👨‍👩‍👧‍👦".chars().count(), 0);
}

#[test]
fn limits_from_configuration_are_made_workable() {
    let limits = OutputLimits::new(10, 0, 5);
    assert_eq!(
        (
            limits.max_bytes(),
            limits.max_lines(),
            limits.max_line_chars()
        ),
        (4096, 8, 256)
    );
    // A line may take a quarter of the bytes at most.
    let limits = OutputLimits::new(8192, 100, 100_000);
    assert_eq!(limits.max_line_chars(), 512);
    let default = OutputLimits::default();
    assert_eq!(
        (
            default.max_bytes(),
            default.max_lines(),
            default.max_line_chars()
        ),
        (32 * 1024, 800, 2000)
    );
    assert_eq!(OutputLimits::new(32 * 1024, 800, 2000), default);
}

#[test]
fn a_budget_counts_lines_and_bytes_and_refuses_what_does_not_fit() {
    let limits = OutputLimits::new(4096, 10, 256);
    let mut budget = limits.budget(2, 96);
    assert_eq!((budget.lines_left(), budget.bytes_left()), (8, 4000));
    assert!(budget.take("twelve bytes"));
    assert_eq!((budget.lines_left(), budget.bytes_left()), (7, 3987));
    // A long line is counted as it will be shown: shortened.
    let long = "x".repeat(5000);
    assert!(budget.take(&long));
    assert_eq!(budget.bytes_left(), 3987 - limits.clip(&long).len() - 1);
    for _ in 0..6 {
        assert!(budget.take(""));
    }
    assert!(!budget.take(""), "the lines are used up");

    let mut tight = limits.budget(0, 4090);
    assert!(!tight.take("too long for six bytes"));
    assert!(tight.take("fits"));
    assert_eq!(tight.limits(), &limits);
}

#[test]
fn similar_lines_are_folded_only_when_the_output_is_over_the_limits() {
    let progress: String = (1..=30).map(|n| format!("Downloading {n}/30\n")).collect();
    let roomy = OutputLimits::default();
    let shaped = shape(
        &progress,
        &roomy,
        Keep::HeadAndTail,
        Compression::Structural,
    );
    assert_eq!(shaped.similar_lines, 0);
    assert_eq!(shaped.text.lines().count(), 30);
    assert!(!shaped.is_lossy());

    let tight = OutputLimits::new(1 << 20, 10, 2000);
    let shaped = shape(
        &progress,
        &tight,
        Keep::HeadAndTail,
        Compression::Structural,
    );
    assert_eq!(
        shaped.text,
        "Downloading 1/30\n[… 28 similar lines not shown …]\nDownloading 30/30"
    );
    assert_eq!(shaped.similar_lines, 28);
    assert!(
        shaped.is_lossy(),
        "folded lines are lost, so the artifact is kept"
    );
    let notice = shaped
        .notice(progress.len(), Err("the disk is full"))
        .expect("a notice");
    assert_eq!(
        notice,
        "[Output cut: 28 similar lines folded. The whole output (531 bytes) could not be kept: \
         the disk is full]"
    );
}

#[test]
fn identical_lines_are_counted_and_nothing_is_lost() {
    let text = format!("start\n{}end\n", "waiting\n".repeat(50));
    let shaped = shape(
        &text,
        &OutputLimits::default(),
        Keep::Head,
        Compression::Structural,
    );
    assert_eq!(
        shaped.text,
        "start\nwaiting\n[the line above repeats 49 more times]\nend"
    );
    assert_eq!(shaped.repeated_lines, 49);
    assert!(!shaped.is_lossy());

    // Content that was asked for is never compressed.
    let exact = shape(
        &text,
        &OutputLimits::default(),
        Keep::Head,
        Compression::None,
    );
    assert_eq!(exact.text.lines().count(), 52);
}

#[test]
fn the_longest_notice_fits_the_room_kept_for_it() {
    let shaped = Shaped {
        text: String::new(),
        pieces: Vec::new(),
        total_lines: 999_999_999,
        omitted_lines: 999_999_998,
        omitted_bytes: 0,
        clipped_lines: 999_999,
        repeated_lines: 0,
        similar_lines: 999_999_999,
    };
    let artifact = ArtifactRef {
        id: ArtifactId::new("a_0123456789abcdefghijkl"),
        label: String::new(),
        bytes: None,
    };
    let kept = shaped.notice(usize::MAX, Ok(&artifact)).expect("a notice");
    let problem = "ü".repeat(500);
    let lost = shaped.notice(usize::MAX, Err(&problem)).expect("a notice");
    for notice in [kept, lost] {
        assert!(
            notice.chars().count() <= MIN_LINE_CHARS,
            "{} characters",
            notice.chars().count()
        );
        assert!(notice.len() <= NOTICE_BYTES, "{} bytes", notice.len());
    }
}

#[test]
fn a_result_over_the_limits_is_cut_stored_and_says_so() {
    let store = MemoryStore::new();
    let limits = OutputLimits::new(1 << 20, 12, 2000);
    let text = varied(500);
    let result = enforce(
        ToolResult::success(text.clone()),
        "docs/find",
        &limits,
        None,
        &store,
    );
    assert_eq!(result.outcome(), &ToolOutcome::Success);
    let shown = result.text();
    assert!(
        shown.starts_with("beta 1\ngamma 2\n[… 490 lines not shown …]\nbeta 493\n"),
        "{shown}"
    );
    assert!(
        shown.ends_with(&format!(
            "gamma 500\n[Output cut: 490 of 500 lines not shown. The whole output ({}) is \
             artifact a_0001: pass that id as `artifact` to read or search.]",
            size(text.len())
        )),
        "{shown}"
    );
    assert_eq!(shown.lines().count(), 12);
    let [artifact] = result.artifacts() else {
        panic!("one artifact, not {:?}", result.artifacts());
    };
    assert_eq!(artifact.label, "full output of docs/find");
    assert_eq!(store.artifact(&artifact.id), Some(text.into_bytes()));
    assert_eq!(
        store
            .artifact_info(&artifact.id)
            .map(|(_, media_type)| media_type),
        Some("text/plain".to_owned())
    );
}

#[test]
fn an_error_result_is_bounded_like_any_other() {
    let store = MemoryStore::new();
    let limits = OutputLimits::new(1 << 20, 12, 2000);
    let result = enforce(
        ToolResult::error(varied(500)),
        "shell",
        &limits,
        None,
        &store,
    );
    assert_eq!(result.outcome(), &ToolOutcome::Error);
    assert!(result.is_bounded());
    assert_eq!(result.artifacts().len(), 1);
    assert!(limits.fits(&result.text()));
}

#[test]
fn a_store_that_fails_still_leaves_a_cut_result_that_says_why() {
    let limits = OutputLimits::new(1 << 20, 12, 2000);
    let store = Unavailable("no space left on device".to_owned());
    let text = varied(500);
    let result = enforce(
        ToolResult::success(text.clone()),
        "shell",
        &limits,
        None,
        &store,
    );
    assert!(result.is_bounded());
    assert!(result.artifacts().is_empty());
    assert!(
        result.text().ends_with(&format!(
            "[Output cut: 490 of 500 lines not shown. The whole output ({}) could not be \
             kept: the content store failed: no space left on device]",
            size(text.len())
        )),
        "{}",
        result.text()
    );
}

#[test]
fn a_result_that_fits_passes_unchanged() {
    let store = MemoryStore::new();
    let result = ToolResult::success("src/main.rs (3 lines)\nfn main() {}")
        .with_payload(serde_json::json!({"lines": 3}));
    let passed = enforce(
        result.clone(),
        "read",
        &OutputLimits::default(),
        None,
        &store,
    );
    assert!(passed.is_bounded());
    assert_eq!(passed.content(), result.content());
    assert_eq!(passed.payload(), result.payload());
    assert_eq!(store.artifact_count(), 0);
}

#[test]
fn an_image_the_model_cannot_take_is_replaced_by_a_line_saying_so() {
    let store = MemoryStore::new();
    let limits = OutputLimits::default();
    let image = |media_type: &str, data: &str| {
        Part::Image(ImagePart {
            media_type: media_type.to_owned(),
            data: Some(data.to_owned()),
            artifact: None,
        })
    };
    let result = ToolResult::success("a chart").with_part(image("image/png", &"A".repeat(4000)));

    // A model that takes no images.
    let none = enforce(result.clone(), "plot", &limits, None, &store);
    assert_eq!(
        none.text(),
        "a chart\n[An image was left out (image/png, 2.9 KiB): the model in use does not take it.]"
    );
    assert_eq!(none.content().len(), 1);

    // One that takes it.
    let support = ImageSupport {
        media_types: vec!["image/png".to_owned()],
        max_bytes: 3000,
    };
    let kept = enforce(result.clone(), "plot", &limits, Some(&support), &store);
    assert_eq!(kept.content(), result.content());

    // One that takes smaller ones, or other types.
    let small = ImageSupport {
        media_types: vec!["image/png".to_owned()],
        max_bytes: 2999,
    };
    assert_eq!(
        enforce(result, "plot", &limits, Some(&small), &store)
            .content()
            .len(),
        1
    );
    let other = ToolResult::success("a chart").with_part(image("image/tiff", "AAAA"));
    assert!(
        enforce(other, "plot", &limits, Some(&support), &store)
            .text()
            .contains("image/tiff, 3 bytes")
    );
}

#[test]
fn numbers_and_sizes_are_written_for_people() {
    assert_eq!(count(0), "0");
    assert_eq!(count(999), "999");
    assert_eq!(count(1000), "1,000");
    assert_eq!(count(1_234_567), "1,234,567");
    assert_eq!(size(0), "0 bytes");
    assert_eq!(size(1), "1 byte");
    assert_eq!(size(1023), "1023 bytes");
    assert_eq!(size(1024), "1.0 KiB");
    assert_eq!(size(32 * 1024), "32.0 KiB");
    assert_eq!(size(1_300_000), "1.2 MiB");
    assert_eq!(size(5 * 1024 * 1024 * 1024), "5.0 GiB");
}
