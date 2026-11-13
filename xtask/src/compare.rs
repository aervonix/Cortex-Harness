//! Two runs' tier times side by side: `cargo xtask ci --tiers --compare A
//! --against B` (`docs/testing-and-ci.md`, "A CI run's test times").
//!
//! A tier near its budget used to be cut by guesswork: nothing said whether
//! a branch had made it slower than the main branch, or which crate and
//! which tests the time had gone to. This reads two JUnit reports, holds
//! each against its own start cost (a slow runner starts every test later,
//! and that is not the branch's doing), and prints each tier on both sides,
//! each crate's change, and the tests that moved most.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::junit::TestCase;
use crate::tiers::{self, Os, Tier};

/// One side of a comparison: a report and what to call it.
pub(crate) struct Side {
    /// `run 1234 (main at abc1234)`, or a file's path.
    pub(crate) label: String,
    pub(crate) cases: Vec<TestCase>,
}

/// How many crates the comparison lists by name.
const CRATES_SHOWN: usize = 12;

/// A change smaller than this, in seconds, is no change: the report shows
/// milliseconds, and a time less its start cost is not exact in binary.
const SAME: f64 = 0.0005;

/// `seconds` rounded to a tenth, with no minus sign on a zero: a change
/// of a few millionths of a second is shown as `+0.0`, not `-0.0`.
fn tenths(seconds: f64) -> f64 {
    let rounded = (seconds * 10.0).round() / 10.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// The comparison of `new` with `old`, for the terminal: each tier above the
/// start on both sides, each crate's change, and each tier's `top` tests
/// that moved most.
pub(crate) fn compare(new: &Side, old: &Side, os: Os, top: usize) -> String {
    let (new_start, new_sample) = tiers::start_of(&new.cases);
    let (old_start, old_sample) = tiers::start_of(&old.cases);
    let mut text = format!(
        "  new: {}\n  old: {}\n  process start: new {:.0} ms a test, old {:.0} ms (each side's 1st \
         percentile of {} and {} unit and integration tests), taken off each side's tests\n",
        new.label,
        old.label,
        1000.0 * new_start,
        1000.0 * old_start,
        new_sample,
        old_sample,
    );
    if new_sample.min(old_sample) < 500 {
        text.push_str(
            "  One side has fewer than 500 unit and integration tests: its start cost may be more \
             than the start, and its time above the start too low.\n",
        );
    }
    let _ = writeln!(
        text,
        "\n  {:<15}  {:>18}  {:>18}  {:>9}  {:>6}",
        "tier", "new above start", "old above start", "change", "budget"
    );
    for tier in Tier::ALL {
        let now = tiers::total(&new.cases, tier, new_start);
        let before = tiers::total(&old.cases, tier, old_start);
        let _ = writeln!(
            text,
            "  {:<15}  {:>9.1} s ({:>4})  {:>9.1} s ({:>4})  {:>+7.1} s  {:>4.0} s",
            tier.name(),
            now.above_start,
            now.count,
            before.above_start,
            before.count,
            tenths(now.above_start - before.above_start),
            tier.budget(os),
        );
    }

    text.push_str(&crates(new, old, new_start, old_start));

    for tier in Tier::ALL {
        let moved = moved(new, old, new_start, old_start, tier);
        if moved.is_empty() || top == 0 {
            continue;
        }
        let _ = writeln!(
            text,
            "\n  moved most in tier {} (time above start, old -> new)",
            tier.name()
        );
        for change in moved.iter().take(top) {
            let what = match (change.old, change.new) {
                (Some(old), Some(new)) => format!("{old:.3} -> {new:.3} s"),
                (None, Some(_)) => "new test".to_owned(),
                (Some(_), None) => "gone".to_owned(),
                (None, None) => String::new(),
            };
            let _ = writeln!(
                text,
                "    {:>+8.3} s  {}  {}  ({what})",
                change.delta, change.binary, change.name
            );
        }
    }
    text
}

/// Each crate's change, tier by tier, largest change first.
fn crates(new: &Side, old: &Side, new_start: f64, old_start: f64) -> String {
    let mut rows: BTreeMap<String, ([f64; 3], [f64; 3])> = BTreeMap::new();
    for (name, seconds) in tiers::by_crate(&new.cases, new_start) {
        rows.entry(name).or_default().0 = seconds;
    }
    for (name, seconds) in tiers::by_crate(&old.cases, old_start) {
        rows.entry(name).or_default().1 = seconds;
    }
    let mut rows: Vec<(String, [f64; 3])> = rows
        .into_iter()
        .map(|(name, (now, before))| {
            (
                name,
                [now[0] - before[0], now[1] - before[1], now[2] - before[2]],
            )
        })
        .collect();
    let size = |change: &[f64; 3]| change.iter().map(|each| each.abs()).sum::<f64>();
    rows.sort_by(|(a_name, a), (b_name, b)| {
        size(b).total_cmp(&size(a)).then_with(|| a_name.cmp(b_name))
    });
    let mut text =
        String::from("\n  change by crate, above start      unit  integration  system\n");
    for (name, change) in rows.iter().take(CRATES_SHOWN) {
        let _ = writeln!(
            text,
            "    {name:<28} {:>+7.1}  {:>+11.1}  {:>+6.1}",
            tenths(change[0]),
            tenths(change[1]),
            tenths(change[2])
        );
    }
    if rows.len() > CRATES_SHOWN {
        let _ = writeln!(
            text,
            "    and {} more crates, each changed less",
            rows.len() - CRATES_SHOWN
        );
    }
    text
}

/// A test's time on each side, above that side's start.
#[derive(Debug, PartialEq)]
struct Moved {
    binary: String,
    name: String,
    old: Option<f64>,
    new: Option<f64>,
    delta: f64,
}

/// A tier's tests with their change, largest change first, either way. A
/// test on one side only counts its whole time. A test that a stress run
/// repeated is counted once, by its slowest repeat.
fn moved(new: &Side, old: &Side, new_start: f64, old_start: f64, tier: Tier) -> Vec<Moved> {
    let times = |cases: &[TestCase], start: f64| {
        let mut times: BTreeMap<(String, String), f64> = BTreeMap::new();
        for case in cases {
            if Tier::of(&case.binary, &case.name) != tier {
                continue;
            }
            let above = (case.seconds - start).max(0.0);
            let time = times
                .entry((case.binary.clone(), case.name.clone()))
                .or_insert(above);
            *time = time.max(above);
        }
        times
    };
    let now = times(&new.cases, new_start);
    let before = times(&old.cases, old_start);
    let mut keys: Vec<&(String, String)> = now.keys().chain(before.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut moved: Vec<Moved> = keys
        .into_iter()
        .map(|key| {
            let (new, old) = (now.get(key).copied(), before.get(key).copied());
            Moved {
                binary: key.0.clone(),
                name: key.1.clone(),
                old,
                new,
                delta: new.unwrap_or(0.0) - old.unwrap_or(0.0),
            }
        })
        .filter(|change| change.delta.abs() >= SAME)
        .collect();
    moved.sort_by(|a, b| {
        b.delta
            .abs()
            .total_cmp(&a.delta.abs())
            .then_with(|| a.binary.cmp(&b.binary))
            .then_with(|| a.name.cmp(&b.name))
    });
    moved
}

#[cfg(test)]
mod tests {
    use super::{Moved, Side, compare, moved};
    use crate::junit;
    use crate::tiers::{Os, Tier};

    /// Two reports of one made-up workspace: the branch made one
    /// integration test slower, added a unit test and removed another, and
    /// ran on a runner that starts a test 10 ms later.
    const MAIN: &str = include_str!("../fixtures/tiers/compare-main.xml");
    const BRANCH: &str = include_str!("../fixtures/tiers/compare-branch.xml");

    fn sides() -> (Side, Side) {
        (
            Side {
                label: "branch".to_owned(),
                cases: junit::parse(BRANCH),
            },
            Side {
                label: "main".to_owned(),
                cases: junit::parse(MAIN),
            },
        )
    }

    #[test]
    fn the_comparison_of_two_fixed_reports_is_exactly_this() {
        let (branch, main) = sides();
        assert_eq!(
            compare(&branch, &main, Os::Linux, 2),
            "  new: branch
  old: main
  process start: new 20 ms a test, old 10 ms (each side's 1st percentile of 7 and 7 unit and \
integration tests), taken off each side's tests
  One side has fewer than 500 unit and integration tests: its start cost may be more than the \
start, and its time above the start too low.

  tier                new above start     old above start     change  budget
  1, unit                1.2 s (   5)        1.0 s (   5)     +0.2 s    20 s
  2, integration         3.5 s (   2)        1.5 s (   2)     +2.0 s    40 s
  3, system              2.0 s (   1)        2.0 s (   1)     +0.0 s    60 s

  change by crate, above start      unit  integration  system
    cortex-llm                      +0.0         +2.0    +0.0
    cortex-base                     +0.2         +0.0    +0.0
    cortex-cli                      +0.0         +0.0    +0.0

  moved most in tier 1, unit (time above start, old -> new)
      +0.600 s  cortex-base  text::tests::new_one  (new test)
      -0.400 s  cortex-base  text::tests::old_one  (gone)

  moved most in tier 2, integration (time above start, old -> new)
      +2.000 s  cortex-llm::it  replay::every_cut  (1.000 -> 3.000 s)
"
        );
    }

    #[test]
    fn a_slower_runner_alone_moves_nothing() {
        // The same tests, each 10 ms slower to start: nothing moved.
        let (branch, main) = sides();
        let changes = moved(&branch, &main, 0.020, 0.010, Tier::System);
        assert_eq!(changes, [], "the system test took 10 ms longer to start");
        let integration = moved(&branch, &main, 0.020, 0.010, Tier::Integration);
        // The quick integration test moved only by the start.
        assert_eq!(integration.len(), 1, "{integration:?}");
        let Moved {
            binary,
            name,
            old,
            new,
            delta,
        } = &integration[0];
        assert_eq!(
            (binary.as_str(), name.as_str()),
            ("cortex-llm::it", "replay::every_cut")
        );
        assert!((old.unwrap() - 1.0).abs() < 1e-9 && (new.unwrap() - 3.0).abs() < 1e-9);
        assert!((delta - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_report_compared_with_itself_has_moved_nowhere() {
        let main = Side {
            label: "a".to_owned(),
            cases: junit::parse(MAIN),
        };
        let again = Side {
            label: "b".to_owned(),
            cases: junit::parse(MAIN),
        };
        let text = compare(&again, &main, Os::Windows, 5);
        assert!(!text.contains("moved most"), "{text}");
        assert!(
            text.contains("+0.0 s    90 s"),
            "the Windows budget: {text}"
        );
    }

    #[test]
    fn two_real_runs_of_one_tree_compare_by_crate() {
        // The fast and the slow Windows runner of one tree: the unit tier
        // reads alike above the start (docs/testing-and-ci.md).
        let fast = Side {
            label: "fast".to_owned(),
            cases: junit::parse(include_str!("../fixtures/tiers/windows-fast-runner.xml")),
        };
        let slow = Side {
            label: "slow".to_owned(),
            cases: junit::parse(include_str!("../fixtures/tiers/windows-slow-runner.xml")),
        };
        let text = compare(&slow, &fast, Os::Windows, 3);
        let unit = text.lines().find(|line| line.contains("1, unit")).unwrap();
        let change: f64 = unit
            .split_whitespace()
            .rev()
            .nth(3)
            .and_then(|change| change.parse().ok())
            .unwrap_or_else(|| panic!("{unit}"));
        assert!(change.abs() < 1.0, "{text}");
        assert!(text.contains("change by crate"), "{text}");
        assert!(text.matches("moved most in tier").count() == 3, "{text}");
    }
}
