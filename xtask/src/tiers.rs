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

    let names = |wanted: Verdict| -> Vec<&str> {
        let tiers = over.iter().filter(|(_, verdict)| *verdict == wanted);
        tiers.map(|(tier, _)| tier.name()).collect()
    };
    let in_actions = std::env::var_os("GITHUB_ACTIONS").is_some();
    const ADVICE: &str = "Make the tests faster or move the slow ones to a non-blocking tier \
                          (docs/testing-and-ci.md). Budgets are not loosened quietly.";

    let warned = names(Verdict::OverBudget);
    if !warned.is_empty() {
        let message = format!(
            "over its time budget: tier {}. {ADVICE}",
            warned.join(", tier ")
        );
        // An annotation, so that it shows on the pull request and not only in the log.
        if in_actions {
            println!("::warning title=Test tier over budget::{message}");
        } else {
            println!("warning: {message}");
        }
    }
    let failed = names(Verdict::Failing);
    if failed.is_empty() {
        return Ok(());
    }
    let message = format!(
        "more than {FAIL_AT} times over its time budget: tier {}. {ADVICE}",
        failed.join(", tier ")
    );
    if in_actions {
        println!("::error title=Test tier far over budget::{message}");
    }
    Err(message)
}

/// A tier fails the job only when it is this many times over its budget.
/// Hosted runners vary by tens of percent from one run to the next, more on
/// Windows, so a failure at exactly the budget would turn a tier that is
/// near its limit into red builds at random. Between the budget and this
/// factor, the job passes with a warning on the pull request.
const FAIL_AT: f64 = 1.5;

/// What a tier's total says about it, when it is over budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Over the budget: a warning.
    OverBudget,
    /// Over [`FAIL_AT`] times the budget: a failure.
    Failing,
}

fn verdict(seconds: f64, budget: f64) -> Option<Verdict> {
    if seconds > FAIL_AT * budget {
        Some(Verdict::Failing)
    } else if seconds > budget {
        Some(Verdict::OverBudget)
    } else {
        None
    }
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
fn summarise(cases: &[TestCase]) -> (String, Vec<(Tier, Verdict)>) {
    let mut table = String::from("  tier             tests   time    budget  used\n");
    let mut over = Vec::new();
    for tier in Tier::ALL {
        let (seconds, count) = total(cases, tier);
        let used = 100.0 * seconds / tier.budget();
        let verdict = verdict(seconds, tier.budget());
        let mark = match verdict {
            None => "",
            Some(Verdict::OverBudget) => "  over budget (warning)",
            Some(Verdict::Failing) => "  FAR OVER BUDGET",
        };
        let _ = writeln!(
            table,
            "  {:<15}  {count:>5}  {seconds:>5.1} s  {:>4.0} s  {used:>3.0}%{mark}",
            tier.name(),
            tier.budget(),
        );
        if let Some(verdict) = verdict {
            over.push((tier, verdict));
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
    use super::{TestCase, Tier, Verdict, parse, summarise, verdict};

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
        assert!(!table.to_lowercase().contains("over budget"), "{table}");
    }

    /// Five unit tests of `seconds` each, and one quick integration test.
    fn unit_tests_taking(seconds: f64) -> Vec<TestCase> {
        let mut cases: Vec<TestCase> = (0..5)
            .map(|i| case("cortex-base", &format!("tests::slow_{i}"), seconds))
            .collect();
        cases.push(case("cortex-agent::it", "loop_::quick", 0.1));
        cases
    }

    #[test]
    fn a_tier_over_its_budget_is_a_warning() {
        // 22.5 s against a budget of 20 s.
        let (table, over) = summarise(&unit_tests_taking(4.5));

        assert_eq!(over, [(Tier::Unit, Verdict::OverBudget)]);
        assert!(table.contains("over budget (warning)"), "{table}");
    }

    #[test]
    fn a_tier_far_over_its_budget_fails() {
        // 32.5 s: more than one and a half times the budget of 20 s.
        let (table, over) = summarise(&unit_tests_taking(6.5));

        assert_eq!(over, [(Tier::Unit, Verdict::Failing)]);
        assert!(table.contains("FAR OVER BUDGET"), "{table}");
    }

    #[test]
    fn the_line_between_a_warning_and_a_failure_is_one_and_a_half_budgets() {
        assert_eq!(verdict(20.0, 20.0), None);
        assert_eq!(verdict(20.1, 20.0), Some(Verdict::OverBudget));
        assert_eq!(verdict(30.0, 20.0), Some(Verdict::OverBudget));
        assert_eq!(verdict(30.1, 20.0), Some(Verdict::Failing));
    }
}
