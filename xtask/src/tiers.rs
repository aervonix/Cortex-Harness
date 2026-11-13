//! The test-time budget of each tier, checked after a CI test run from the
//! JUnit report nextest writes (`docs/testing-and-ci.md`, "Test tiers").
//!
//! A single test is held to its limit by nextest itself (`.config/nextest.toml`
//! stops a test that runs too long). This is the other half: the time all the
//! tests of a tier take together, and which tests that time is made of.
//!
//! **A budget is held against the time above the start of the tests'
//! processes.** nextest runs every test in a process of its own, and on a
//! hosted Windows runner starting one costs 6 to 12 ms, by the runner's
//! speed. For a thousand unit tests that is 6 to 12 s of a 20 s budget before
//! any test has done anything, so the raw total said more about the runner a
//! run drew than about the tests. Each run measures its own start cost
//! ([`start_cost`]) and takes it off every test; the report prints both
//! totals, and the cost it used.

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

/// Which of a run's unit and integration test times is taken as the cost of
/// starting one test's process: the time that this many in a hundred of
/// them were quicker than.
///
/// The quickest tests of a run do next to nothing, so their time is the
/// start of a process. Not the very quickest: one test that happens to be
/// a millisecond quicker than the rest moves the whole floor, and with it a
/// thousand tests' worth of time. In one job that ran one tree twice with
/// each of two settings, the unit tier's figure moved by 1.1 s between the
/// two runs of one setting with the minimum as the floor, where the 1st
/// percentile and the raw totals did not move at all. System tests are not
/// looked at: they start other processes of their own.
const START_PERCENTILE: usize = 1;

/// Below this many unit and integration tests, the report says that its
/// start cost may be more than the start: in a run of a few crates, or of
/// tests chosen by a filter, the quickest may all do real work.
const SMALL_SAMPLE: usize = 500;

/// What starting one test's process cost in this run, in seconds: the
/// [`START_PERCENTILE`]th percentile of its unit and integration tests'
/// times. A run with none of those has no floor to measure: 0. The report
/// asks [`start_of`], which says how many tests it was measured on too.
#[cfg(test)]
fn start_cost(cases: &[TestCase]) -> f64 {
    start_of(cases).0
}

/// The start cost, and how many unit and integration tests it was
/// measured on.
fn start_of(cases: &[TestCase]) -> (f64, usize) {
    let mut times: Vec<f64> = cases
        .iter()
        .filter(|case| Tier::of(&case.binary, &case.name) != Tier::System)
        .map(|case| case.seconds)
        .collect();
    if times.is_empty() {
        return (0.0, 0);
    }
    times.sort_by(f64::total_cmp);
    (times[times.len() * START_PERCENTILE / 100], times.len())
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
    /// times of its tests above the start of their processes, not the wall
    /// time of the run.
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

/// What a tier's tests took together.
#[derive(Clone, Copy, Debug)]
struct Totals {
    count: usize,
    /// The sum of the tests' times.
    seconds: f64,
    /// The sum of what each test took above the start cost, a test that
    /// took less counting as nothing. This is what the budget is held to.
    above_start: f64,
}

fn total(cases: &[TestCase], tier: Tier, start: f64) -> Totals {
    let of_tier = cases
        .iter()
        .filter(|case| Tier::of(&case.binary, &case.name) == tier);
    of_tier.fold(
        Totals {
            count: 0,
            seconds: 0.0,
            above_start: 0.0,
        },
        |totals, case| Totals {
            count: totals.count + 1,
            seconds: totals.seconds + case.seconds,
            above_start: totals.above_start + (case.seconds - start).max(0.0),
        },
    )
}

/// The line that says what the start of a test's process was taken to
/// cost, and from what.
fn start_line(start: f64, sample: usize) -> String {
    let mut line = format!(
        "process start: {:.0} ms a test (the {START_PERCENTILE}st percentile of the run's {sample} \
         unit and integration tests' times), taken off every test before a tier is held to its \
         budget",
        1000.0 * start,
    );
    if sample < SMALL_SAMPLE {
        line.push_str(&format!(
            ". It was measured on fewer than {SMALL_SAMPLE} tests: in a run of a few crates, or \
             of tests a filter chose, the quickest may do more than start, and the figures \
             above the start are then too low"
        ));
    }
    line
}

/// The table for the terminal, and the tiers that are over budget.
fn summarise(cases: &[TestCase], os: Os) -> (String, Vec<(Tier, Verdict)>) {
    let (start, sample) = start_of(cases);
    let mut table = format!(
        "  {}\n  tier             tests   time    above start  budget  used\n",
        start_line(start, sample)
    );
    let mut over = Vec::new();
    for tier in Tier::ALL {
        let Totals {
            count,
            seconds,
            above_start,
        } = total(cases, tier, start);
        let budget = tier.budget(os);
        let used = 100.0 * above_start / budget;
        let verdict = verdict(above_start, budget);
        let mark = match verdict {
            None => "",
            Some(Verdict::OverBudget) => "  over budget (warning)",
            Some(Verdict::Failing) => "  FAR OVER BUDGET",
        };
        let _ = writeln!(
            table,
            "  {:<15}  {count:>5}  {seconds:>5.1} s  {above_start:>9.1} s  {budget:>4.0} s  {used:>3.0}%{mark}",
            tier.name(),
        );
        if let Some(verdict) = verdict {
            over.push((tier, verdict));
        }
    }
    (table, over)
}

fn markdown(cases: &[TestCase], os: Os) -> String {
    let (start, sample) = start_of(cases);
    let mut table = format!(
        "The {}.\n\n| Tier | Tests | Time | Above start | Budget | Used |\n\
         |---|---:|---:|---:|---:|---:|\n",
        start_line(start, sample).replacen("process start: ", "start of a test's process: ", 1)
    );
    for tier in Tier::ALL {
        let Totals {
            count,
            seconds,
            above_start,
        } = total(cases, tier, start);
        let budget = tier.budget(os);
        let _ = writeln!(
            table,
            "| {} | {count} | {seconds:.1} s | {above_start:.1} s | {budget:.0} s | {:.0}% |",
            tier.name(),
            100.0 * above_start / budget,
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
        let seconds = total(cases, tier, 0.0).seconds;
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
    use super::{
        Os, Tier, Totals, Verdict, markdown, slowest_markdown, slowest_tests, start_cost,
        summarise, total, verdict,
    };
    use crate::junit::tests::case;
    use crate::junit::{self, TestCase};

    /// A report kept in the repository: thirteen tests across the three
    /// tiers, two of them failures, with times chosen so that every line of
    /// the report below can be checked by hand. Its quickest unit or
    /// integration test took 0.2 s, which is therefore its start cost.
    const REPORT: &str = include_str!("../fixtures/tiers/junit.xml");

    /// Two real reports of one tree, from the Windows job on 2026-11-13:
    /// one from a fast runner and one from a slow one. Only each test's
    /// binary, tier and time are kept.
    const FAST_RUNNER: &str = include_str!("../fixtures/tiers/windows-fast-runner.xml");
    const SLOW_RUNNER: &str = include_str!("../fixtures/tiers/windows-slow-runner.xml");

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
            "  process start: 200 ms a test (the 1st percentile of the run's 9 unit and \
             integration tests' times), taken off every test before a tier is held to its budget. \
             It was measured on fewer than 500 tests: in a run of a few crates, or of tests a \
             filter chose, the quickest may do more than start, and the figures above the start \
             are then too low
  tier             tests   time    above start  budget  used
  1, unit              5    6.0 s        5.0 s    20 s   25%
  2, integration       4   10.0 s        9.2 s    40 s   23%
  3, system            4   15.0 s       14.2 s    60 s   24%

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
            "The start of a test's process: 200 ms a test (the 1st percentile of the run's 9 unit \
             and integration tests' times), taken off every test before a tier is held to its \
             budget. It was measured on fewer than 500 tests: in a run of a few crates, or of \
             tests a filter chose, the quickest may do more than start, and the figures above the \
             start are then too low.

| Tier | Tests | Time | Above start | Budget | Used |
|---|---:|---:|---:|---:|---:|
| 1, unit | 5 | 6.0 s | 5.0 s | 20 s | 25% |
| 2, integration | 4 | 10.0 s | 9.2 s | 40 s | 23% |
| 3, system | 4 | 15.0 s | 14.2 s | 90 s | 16% |
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
        // 22.0 s above the start cost of 0.1 s, against a budget of 20 s.
        let (table, over) = summarise(&unit_tests_taking(4.5), Os::Linux);

        assert_eq!(over, [(Tier::Unit, Verdict::OverBudget)]);
        assert!(table.contains("over budget (warning)"), "{table}");
    }

    #[test]
    fn a_tier_far_over_its_budget_fails() {
        // 32.0 s above the start cost: more than one and a half times the
        // budget of 20 s.
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
    fn the_start_cost_is_what_one_in_a_hundred_unit_and_integration_tests_beat() {
        // Two hundred tests from 10 ms to 209 ms: two of them are quicker
        // than 12 ms.
        let mut cases: Vec<TestCase> = (0..200)
            .map(|i| {
                case(
                    "cortex-base",
                    &format!("tests::t{i}"),
                    0.010 + 0.001 * f64::from(i),
                )
            })
            .collect();
        assert!((start_cost(&cases) - 0.012).abs() < 1e-9);
        // A system test is not looked at, however quick: it starts other
        // processes, and is no measure of starting one.
        cases.push(case("cortex-tools::it", "shell::sys_quick", 0.001));
        assert!((start_cost(&cases) - 0.012).abs() < 1e-9);
        // With fewer than a hundred, the quickest.
        let few = [
            case("cortex-base", "tests::a", 0.030),
            case("cortex-agent::it", "rounds::b", 0.020),
        ];
        assert!((start_cost(&few) - 0.020).abs() < 1e-9);
        // With none to measure, nothing is taken off.
        let system_only = [case("cortex-cli::system", "sys_version", 0.5)];
        assert_eq!(start_cost(&system_only), 0.0);
        assert_eq!(start_cost(&[]), 0.0);
    }

    #[test]
    fn a_test_quicker_than_the_start_cost_counts_as_nothing() {
        let cases = [
            case("cortex-base", "tests::a", 0.010),
            case("cortex-base", "tests::b", 0.008),
            case("cortex-base", "tests::c", 0.510),
        ];
        let totals = total(&cases, Tier::Unit, 0.010);
        assert_eq!(totals.count, 3);
        assert!((totals.seconds - 0.528).abs() < 1e-9);
        assert!((totals.above_start - 0.5).abs() < 1e-9, "{totals:?}");
    }

    #[test]
    fn a_slow_runner_alone_does_not_put_a_tier_over_its_budget() {
        // Two thousand unit tests that do nothing, each 12 ms of starting
        // a process on a slow runner: 24 s, over the budget of 20 s as a
        // raw total, and nothing above the start.
        let cases: Vec<TestCase> = (0..2000)
            .map(|i| case("cortex-base", &format!("tests::t{i}"), 0.012))
            .collect();
        let (table, over) = summarise(&cases, Os::Windows);
        assert_eq!(over, [], "{table}");
        assert!(table.contains("24.0 s        0.0 s"), "{table}");
        // Two thousand tests are enough to measure the start on.
        assert!(!table.contains("fewer than"), "{table}");
        // One test that takes 21 s puts it over, as it should.
        let mut slow = cases;
        slow.push(case("cortex-base", "tests::slow", 21.0));
        assert_eq!(
            summarise(&slow, Os::Windows).1,
            [(Tier::Unit, Verdict::OverBudget)]
        );
    }

    /// Each tier's raw total and its total above the start cost.
    fn tiers_of(report: &str) -> (f64, Vec<Totals>) {
        let cases = junit::parse(report);
        let start = start_cost(&cases);
        let totals = Tier::ALL
            .iter()
            .map(|tier| total(&cases, *tier, start))
            .collect();
        (start, totals)
    }

    /// How far apart two figures are, as a share of the smaller.
    fn apart(a: f64, b: f64) -> f64 {
        (a - b).abs() / a.min(b)
    }

    #[test]
    fn a_fast_and_a_slow_windows_runner_read_alike_above_the_start() {
        let (fast_start, fast) = tiers_of(FAST_RUNNER);
        let (slow_start, slow) = tiers_of(SLOW_RUNNER);
        // The two runners: 6 ms and 11 ms to start a test.
        assert!((fast_start - 0.006).abs() < 1e-9, "{fast_start}");
        assert!((slow_start - 0.011).abs() < 1e-9, "{slow_start}");
        let (unit, integration, system) = (0, 1, 2);
        assert_eq!(fast[unit].count, slow[unit].count);

        // The unit tier: 17.0 s and 22.7 s raw, a third apart, which is
        // the runner and not the tests. Above the start, 11.0 s and 11.6 s:
        // within the run-to-run noise of one runner and one setting (0.5 s
        // in the one job that measured it).
        assert!(apart(fast[unit].seconds, slow[unit].seconds) > 0.30);
        assert!(
            apart(fast[unit].above_start, slow[unit].above_start) < 0.10,
            "{fast:?} {slow:?}"
        );

        // The integration tier: 28.2 s and 40.6 s raw, 23.1 s and 31.3 s
        // above the start. The start explains part of the gap, and the
        // rest is the slow runner being slower at the tests' work, which
        // the budget is right to see: its tests do much more than start.
        assert!(
            apart(fast[integration].above_start, slow[integration].above_start)
                < apart(fast[integration].seconds, slow[integration].seconds)
        );
        assert!(slow[integration].above_start < 40.0);

        // The system tier is unchanged in substance: its tests take
        // seconds, of which the start is a sliver.
        assert!(slow[system].seconds - slow[system].above_start < 1.0);
    }

    #[test]
    fn the_line_between_a_warning_and_a_failure_is_one_and_a_half_budgets() {
        assert_eq!(verdict(20.0, 20.0), None);
        assert_eq!(verdict(20.1, 20.0), Some(Verdict::OverBudget));
        assert_eq!(verdict(30.0, 20.0), Some(Verdict::OverBudget));
        assert_eq!(verdict(30.1, 20.0), Some(Verdict::Failing));
    }
}
