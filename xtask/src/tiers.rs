//! The test-time budget of each tier, checked after a CI test run from the
//! JUnit report nextest writes (`docs/testing-and-ci.md`, "Test tiers").
//!
//! A single test is held to its limit by nextest itself (`.config/nextest.toml`
//! stops a test that runs too long). This is the other half: the time all the
//! tests of a tier take together.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::Result;
use crate::steps;
use crate::workspace;

/// A test tier that blocks pull requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Unit,
    Integration,
    System,
}

impl Tier {
    const ALL: [Tier; 3] = [Tier::Unit, Tier::Integration, Tier::System];

    fn name(self) -> &'static str {
        match self {
            Tier::Unit => "1, unit",
            Tier::Integration => "2, integration",
            Tier::System => "3, system",
        }
    }

    /// The budget for the tier's total test time, in seconds: the sum of the
    /// times of its tests, not the wall time of the run.
    fn budget(self) -> f64 {
        match self {
            Tier::Unit => 20.0,
            Tier::Integration => 40.0,
            Tier::System if cfg!(windows) => 90.0,
            Tier::System => 60.0,
        }
    }

    /// Where a test lives, and its name, decide its tier.
    fn of(binary: &str, test: &str) -> Tier {
        if test.starts_with("sys_") || test.contains("::sys_") {
            Tier::System
        } else if binary.ends_with("::it") {
            Tier::Integration
        } else {
            Tier::Unit
        }
    }
}

/// One `<testcase>` of the report.
#[derive(Debug, PartialEq)]
struct TestCase {
    /// nextest's binary id: `cortex-base`, `cortex-agent::it`, `cortex-cli::system`.
    binary: String,
    name: String,
    seconds: f64,
}

/// The step that follows the test run in `cargo xtask ci test`.
pub(crate) fn check_ci_report() -> Result<()> {
    let metadata = workspace::metadata(&workspace::root())?;
    let report = metadata
        .target_directory
        .join("nextest")
        .join("ci")
        .join("junit.xml");
    check_report(&report)
}

fn check_report(report: &Path) -> Result<()> {
    let xml = fs::read_to_string(report)
        .map_err(|error| format!("could not read {}: {error}", report.display()))?;
    let cases = parse(&xml);
    if cases.is_empty() {
        return Err(format!("{} lists no tests", report.display()));
    }
    let (table, over) = summarise(&cases);
    print!("{table}");
    steps::append_job_summary(&format!("### Test time by tier\n\n{}\n", markdown(&cases)));
    if over.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = over.iter().map(|tier| tier.name()).collect();
    let message = format!(
        "over its time budget: tier {}. Make the tests faster or move the slow ones to a \
         non-blocking tier (docs/testing-and-ci.md). Budgets are not loosened quietly.",
        names.join(", tier ")
    );
    // An annotation, so that it shows on the pull request and not only in the log.
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        println!("::error title=Test tier over budget::{message}");
    }
    Err(message)
}

/// Total seconds and number of tests for a tier.
fn total(cases: &[TestCase], tier: Tier) -> (f64, usize) {
    let of_tier = cases
        .iter()
        .filter(|case| Tier::of(&case.binary, &case.name) == tier);
    of_tier.fold((0.0, 0), |(seconds, count), case| {
        (seconds + case.seconds, count + 1)
    })
}

/// The table for the terminal, and the tiers that are over budget.
fn summarise(cases: &[TestCase]) -> (String, Vec<Tier>) {
    let mut table = String::from("  tier             tests   time    budget  used\n");
    let mut over = Vec::new();
    for tier in Tier::ALL {
        let (seconds, count) = total(cases, tier);
        let used = 100.0 * seconds / tier.budget();
        let _ = writeln!(
            table,
            "  {:<15}  {count:>5}  {seconds:>5.1} s  {:>4.0} s  {used:>3.0}%{}",
            tier.name(),
            tier.budget(),
            if seconds > tier.budget() {
                "  OVER BUDGET"
            } else {
                ""
            },
        );
        if seconds > tier.budget() {
            over.push(tier);
        }
    }
    (table, over)
}

fn markdown(cases: &[TestCase]) -> String {
    let mut table =
        String::from("| Tier | Tests | Time | Budget | Used |\n|---|---:|---:|---:|---:|\n");
    for tier in Tier::ALL {
        let (seconds, count) = total(cases, tier);
        let _ = writeln!(
            table,
            "| {} | {count} | {seconds:.1} s | {:.0} s | {:.0}% |",
            tier.name(),
            tier.budget(),
            100.0 * seconds / tier.budget(),
        );
    }
    table
}

/// Reads the `<testcase>` elements of a JUnit report. It reads only what
/// nextest writes: three attributes in double quotes on each element.
fn parse(xml: &str) -> Vec<TestCase> {
    let mut cases = Vec::new();
    for element in xml.split("<testcase ").skip(1) {
        let tag = element.split('>').next().unwrap_or(element);
        let seconds = attribute(tag, "time").and_then(|time| time.parse().ok());
        if let (Some(name), Some(binary), Some(seconds)) =
            (attribute(tag, "name"), attribute(tag, "classname"), seconds)
        {
            cases.push(TestCase {
                binary: binary.to_owned(),
                name: name.to_owned(),
                seconds,
            });
        }
    }
    cases
}

/// The value of `name="..."` in a start tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    loop {
        let start = rest.find(&format!("{name}=\""))?;
        // "name" must not be the tail of "classname".
        let whole_name = start == 0 || rest[..start].ends_with(char::is_whitespace);
        let value = &rest[start + name.len() + 2..];
        if whole_name {
            return value.split('"').next();
        }
        rest = value;
    }
}

#[cfg(test)]
mod tests {
    use super::{TestCase, Tier, parse, summarise};

    const REPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="0" errors="0" time="0.9">
    <testsuite name="cortex-base" tests="1" disabled="0" errors="0" failures="0">
        <testcase name="text::tests::wraps_at_the_width" classname="cortex-base" timestamp="2026-11-12T13:25:40.000+00:00" time="0.004">
        </testcase>
    </testsuite>
    <testsuite name="cortex-agent::it" tests="1" disabled="0" errors="0" failures="0">
        <testcase classname="cortex-agent::it" name="loop_::retries_after_a_stall" time="0.250"/>
    </testsuite>
    <testsuite name="cortex-cli::system" tests="2" disabled="0" errors="0" failures="0">
        <testcase name="sys_version_is_byte_identical_under_both_names" classname="cortex-cli::system" time="0.5">
            <system-out>cortex 0.0.0</system-out>
        </testcase>
        <testcase name="rules::tests::sys_the_repository_follows_its_own_rules" classname="xtask::bin/xtask" time="0.125">
        </testcase>
    </testsuite>
</testsuites>
"#;

    fn case(binary: &str, name: &str, seconds: f64) -> TestCase {
        TestCase {
            binary: binary.to_owned(),
            name: name.to_owned(),
            seconds,
        }
    }

    #[test]
    fn the_report_is_read_whatever_the_attribute_order() {
        assert_eq!(
            parse(REPORT),
            [
                case("cortex-base", "text::tests::wraps_at_the_width", 0.004),
                case("cortex-agent::it", "loop_::retries_after_a_stall", 0.25),
                case(
                    "cortex-cli::system",
                    "sys_version_is_byte_identical_under_both_names",
                    0.5
                ),
                case(
                    "xtask::bin/xtask",
                    "rules::tests::sys_the_repository_follows_its_own_rules",
                    0.125
                ),
            ]
        );
    }

    #[test]
    fn where_a_test_lives_and_its_name_decide_its_tier() {
        assert_eq!(Tier::of("cortex-base", "text::tests::wraps"), Tier::Unit);
        assert_eq!(
            Tier::of("cortex-cli::bin/cortex", "tests::help"),
            Tier::Unit
        );
        assert_eq!(
            Tier::of("cortex-agent::it", "loop_::retries"),
            Tier::Integration
        );
        assert_eq!(Tier::of("cortex-cli::system", "sys_version"), Tier::System);
        // A test that starts a process is a system test wherever it lives.
        assert_eq!(
            Tier::of("xtask::bin/xtask", "rules::tests::sys_repository"),
            Tier::System
        );
        assert_eq!(
            Tier::of("cortex-agent::it", "tools::sys_shell_is_cancelled"),
            Tier::System
        );
        // The prefix counts only at the start of a path segment.
        assert_eq!(
            Tier::of("cortex-base", "tests::parses_sys_calls"),
            Tier::Unit
        );
    }

    #[test]
    fn a_run_inside_its_budgets_passes() {
        let (table, over) = summarise(&parse(REPORT));

        assert_eq!(over, []);
        assert!(table.contains("1, unit"), "{table}");
        assert!(table.contains("  0.6 s"), "{table}");
        assert!(!table.contains("OVER BUDGET"), "{table}");
    }

    #[test]
    fn a_tier_over_its_budget_is_named() {
        let mut cases: Vec<TestCase> = (0..5)
            .map(|i| case("cortex-base", &format!("tests::slow_{i}"), 4.5))
            .collect();
        cases.push(case("cortex-agent::it", "loop_::quick", 0.1));

        let (table, over) = summarise(&cases);

        assert_eq!(over, [Tier::Unit]);
        assert!(table.contains("OVER BUDGET"), "{table}");
    }
}
