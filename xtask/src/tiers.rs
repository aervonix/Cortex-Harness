//! The test-time budget of each tier, checked after a CI test run from the
//! JUnit report nextest writes (`docs/testing-and-ci.md`, "Test tiers").
//!
//! A single test is held to its limit by nextest itself (`.config/nextest.toml`
//! stops a test that runs too long). This is the other half: the time all the
//! tests of a tier take together, and which tests that time is made of.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::junit::{self, TestCase};
use crate::steps;
use crate::workspace;

/// How many of each tier's slowest tests a CI run lists.
const SLOWEST_IN_CI: usize = 5;

/// A test tier that blocks pull requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Unit,
    Integration,
    System,
}

/// The OS a report is from. Tier 3 has a larger budget on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    Linux,
    Windows,
}

impl Os {
    /// The OS this command is running on.
    pub(crate) fn here() -> Self {
        if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
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
    fn budget(self, os: Os) -> f64 {
        match (self, os) {
            (Tier::Unit, _) => 20.0,
            (Tier::Integration, _) => 40.0,
            (Tier::System, Os::Windows) => 90.0,
            (Tier::System, Os::Linux) => 60.0,
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

/// Where a run in nextest's `ci` profile leaves its report.
pub(crate) fn ci_report_path() -> PathBuf {
    workspace::nextest_report(&workspace::root(), "ci")
}

/// The step that follows the test run in `cargo xtask ci test`.
pub(crate) fn check_ci_report() -> Result<()> {
    check_report(&ci_report_path(), Os::here(), SLOWEST_IN_CI)
}

/// Prints the tier totals of a report and each tier's `slowest` slowest
/// tests, and fails when a tier is far over its budget.
pub(crate) fn check_report(report: &Path, os: Os, slowest: usize) -> Result<()> {
    let cases = junit::read(report)?;
    let (table, over) = summarise(&cases, os);
    print!("{table}");
    print!("{}", slowest_tests(&cases, slowest));
    steps::append_job_summary(&format!(
        "### Test time by tier\n\n{}\n{}\n",
        markdown(&cases, os),
        slowest_markdown(&cases, slowest)
    ));

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
fn summarise(cases: &[TestCase], os: Os) -> (String, Vec<(Tier, Verdict)>) {
    let mut table = String::from("  tier             tests   time    budget  used\n");
    let mut over = Vec::new();
    for tier in Tier::ALL {
        let (seconds, count) = total(cases, tier);
        let budget = tier.budget(os);
        let used = 100.0 * seconds / budget;
        let verdict = verdict(seconds, budget);
        let mark = match verdict {
            None => "",
            Some(Verdict::OverBudget) => "  over budget (warning)",
            Some(Verdict::Failing) => "  FAR OVER BUDGET",
        };
        let _ = writeln!(
            table,
            "  {:<15}  {count:>5}  {seconds:>5.1} s  {budget:>4.0} s  {used:>3.0}%{mark}",
            tier.name(),
        );
        if let Some(verdict) = verdict {
            over.push((tier, verdict));
        }
    }
    (table, over)
}

fn markdown(cases: &[TestCase], os: Os) -> String {
    let mut table =
        String::from("| Tier | Tests | Time | Budget | Used |\n|---|---:|---:|---:|---:|\n");
    for tier in Tier::ALL {
        let (seconds, count) = total(cases, tier);
        let budget = tier.budget(os);
        let _ = writeln!(
            table,
            "| {} | {count} | {seconds:.1} s | {budget:.0} s | {:.0}% |",
            tier.name(),
            100.0 * seconds / budget,
        );
    }
    table
}

/// The `count` slowest tests of a tier, slowest first. Tests that took the
/// same time are in name order, so the list is the same for the same report.
fn slowest(cases: &[TestCase], tier: Tier, count: usize) -> Vec<&TestCase> {
    let mut of_tier: Vec<&TestCase> = cases
        .iter()
        .filter(|case| Tier::of(&case.binary, &case.name) == tier)
        .collect();
    of_tier.sort_by(|a, b| {
        b.seconds
            .total_cmp(&a.seconds)
            .then_with(|| a.binary.cmp(&b.binary))
            .then_with(|| a.name.cmp(&b.name))
    });
    of_tier.truncate(count);
    of_tier
}

/// Each tier's slowest tests, for the terminal and the CI log. A CI log
/// shows no time for a test under two seconds, and a tier's total is made
/// of hundreds of such tests: this is where its time is.
fn slowest_tests(cases: &[TestCase], count: usize) -> String {
    let mut lines = String::new();
    if count == 0 {
        return lines;
    }
    for tier in Tier::ALL {
        let tests = slowest(cases, tier, count);
        if tests.is_empty() {
            continue;
        }
        let (seconds, _) = total(cases, tier);
        let shown: f64 = tests.iter().map(|case| case.seconds).sum();
        let _ = writeln!(
            lines,
            "\n  slowest in tier {} ({} tests, {:.0}% of the tier's time)",
            tier.name(),
            tests.len(),
            if seconds > 0.0 {
                100.0 * shown / seconds
            } else {
                0.0
            },
        );
        for case in tests {
            let _ = writeln!(
                lines,
                "    {:>7.3} s  {}  {}",
                case.seconds, case.binary, case.name
            );
        }
    }
    lines
}

fn slowest_markdown(cases: &[TestCase], count: usize) -> String {
    if count == 0 {
        return String::new();
    }
    let mut table = format!(
        "The {count} slowest tests of each tier:\n\n| Tier | Seconds | Test |\n|---|---:|---|\n"
    );
    for tier in Tier::ALL {
        for case in slowest(cases, tier, count) {
            let _ = writeln!(
                table,
                "| {} | {:.3} | `{}` `{}` |",
                tier.name(),
                case.seconds,
                case.binary,
                case.name
            );
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::{Os, Tier, Verdict, markdown, slowest_markdown, slowest_tests, summarise, verdict};
    use crate::junit::tests::case;
    use crate::junit::{self, TestCase};

    /// A report kept in the repository: thirteen tests across the three
    /// tiers, two of them failures, with times chosen so that every line of
    /// the report below can be checked by hand.
    const REPORT: &str = include_str!("../fixtures/tiers/junit.xml");

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
    fn the_report_of_a_fixed_run_is_exactly_this() {
        let cases = junit::parse(REPORT);

        let (table, over) = summarise(&cases, Os::Linux);
        let report = format!("{table}{}", slowest_tests(&cases, 3));

        assert_eq!(over, []);
        assert_eq!(
            report,
            "  tier             tests   time    budget  used
  1, unit              5    6.0 s    20 s   30%
  2, integration       4   10.0 s    40 s   25%
  3, system            4   15.0 s    60 s   25%

  slowest in tier 1, unit (3 tests, 92% of the tier's time)
      3.000 s  cortex-policy  rules::tests::a_deny_beats_an_allow_in_any_order
      1.500 s  cortex-base  framing::tests::every_split_into_three
      1.000 s  cortex-base  text::tests::wraps_at_the_width

  slowest in tier 2, integration (3 tests, 98% of the tier's time)
      6.250 s  cortex-llm::it  anthropic::replay::every_fixture_cut_in_two
      2.500 s  cortex-agent::it  denials::a_refused_call_stops_the_turn
      1.050 s  cortex-agent::it  rounds::two_reads_run_together

  slowest in tier 3, system (3 tests, 97% of the tier's time)
     10.000 s  cortex-policy::it  differential::sys_bash_agrees
      2.500 s  cortex-tools::it  shell::sys_the_exit_status_is_the_commands
      2.000 s  xtask::bin/xtask  rules::tests::sys_the_repository_follows_its_own_rules
"
        );
    }

    #[test]
    fn the_summary_page_has_the_same_numbers() {
        let cases = junit::parse(REPORT);

        assert_eq!(
            markdown(&cases, Os::Windows),
            "| Tier | Tests | Time | Budget | Used |
|---|---:|---:|---:|---:|
| 1, unit | 5 | 6.0 s | 20 s | 30% |
| 2, integration | 4 | 10.0 s | 40 s | 25% |
| 3, system | 4 | 15.0 s | 90 s | 17% |
"
        );
        assert_eq!(
            slowest_markdown(&cases, 1),
            "The 1 slowest tests of each tier:

| Tier | Seconds | Test |
|---|---:|---|
| 1, unit | 3.000 | `cortex-policy` `rules::tests::a_deny_beats_an_allow_in_any_order` |
| 2, integration | 6.250 | `cortex-llm::it` `anthropic::replay::every_fixture_cut_in_two` |
| 3, system | 10.000 | `cortex-policy::it` `differential::sys_bash_agrees` |
"
        );
    }

    #[test]
    fn tests_that_took_the_same_time_are_listed_in_name_order() {
        let cases = [
            case("cortex-base", "tests::b", 0.5),
            case("cortex-base", "tests::a", 0.5),
            case("cortex-agent", "tests::z", 0.5),
        ];

        let lines = slowest_tests(&cases, 2);

        let z = lines.find("cortex-agent  tests::z").unwrap();
        let a = lines.find("cortex-base  tests::a").unwrap();
        assert!(z < a, "{lines}");
        assert!(!lines.contains("tests::b"), "{lines}");
    }

    #[test]
    fn asking_for_no_slowest_tests_prints_none() {
        let cases = junit::parse(REPORT);

        assert_eq!(slowest_tests(&cases, 0), "");
        assert_eq!(slowest_markdown(&cases, 0), "");
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
        let (table, over) = summarise(&unit_tests_taking(4.5), Os::Linux);

        assert_eq!(over, [(Tier::Unit, Verdict::OverBudget)]);
        assert!(table.contains("over budget (warning)"), "{table}");
    }

    #[test]
    fn a_tier_far_over_its_budget_fails() {
        // 32.5 s: more than one and a half times the budget of 20 s.
        let (table, over) = summarise(&unit_tests_taking(6.5), Os::Linux);

        assert_eq!(over, [(Tier::Unit, Verdict::Failing)]);
        assert!(table.contains("FAR OVER BUDGET"), "{table}");
    }

    #[test]
    fn the_system_tier_has_more_room_on_windows() {
        // 75 s of system tests: over 60 s, inside 90 s.
        let cases: Vec<TestCase> = (0..5)
            .map(|i| case("cortex-cli::system", &format!("sys_slow_{i}"), 15.0))
            .collect();

        assert_eq!(
            summarise(&cases, Os::Linux).1,
            [(Tier::System, Verdict::OverBudget)]
        );
        assert_eq!(summarise(&cases, Os::Windows).1, []);
    }

    #[test]
    fn the_line_between_a_warning_and_a_failure_is_one_and_a_half_budgets() {
        assert_eq!(verdict(20.0, 20.0), None);
        assert_eq!(verdict(20.1, 20.0), Some(Verdict::OverBudget));
        assert_eq!(verdict(30.0, 20.0), Some(Verdict::OverBudget));
        assert_eq!(verdict(30.1, 20.0), Some(Verdict::Failing));
    }
}
