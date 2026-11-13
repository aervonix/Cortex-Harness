//! `cargo xtask stress`: run tests many times over, each time under another
//! seed, and report the ones that fail on some
//! (`docs/testing-and-ci.md`, "A seeded order of events").
//!
//! A test that passes once may depend on an order of events that the run
//! happened to have. Two kinds of test are worth repeating:
//!
//! - tests whose rig seeds the order (`cortex-agent`'s): repeat `n` runs
//!   with seed `first + n`, so each repeat tries another order, and a
//!   failure names the seed that brings it back;
//! - tests that start processes or cross threads, where the machine decides
//!   the order: repeating is all there is, and the seed means nothing.
//!
//! The repeating is nextest's (`--stress-count`). This sets the first seed,
//! reads which repeat of which test failed from the JUnit report, and says
//! how to run a failure again.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::Command;

use crate::Result;
use crate::checks;
use crate::junit::{self, TestCase};
use crate::load::Squeeze;
use crate::workspace;

const USAGE: &str = "\
usage: cargo xtask stress -p CRATE [-p CRATE]... [--count N] [--first-seed S]
                          [--load N] [--cores K] [NEXTEST ARGUMENT]...

Runs the crates' tests N times over (100 when not given). Repeat n runs with
seed S + n (S is 0 when not given). Anything else goes to `cargo nextest run`:
a filter, `-E`, `--run-ignored`.

--cores K pins the run to K cores and --load N runs N busy loops beside it,
on the same cores: a race that needs the machine to be slow at the wrong
moment shows far more often. The tests are built first, unpinned.

A test that failed is run again in the same order with
    CORTEX_TEST_SEED=<seed> cargo xtask test -p <crate> -E 'test(=<name>)'";

/// The variable a seeded test rig reads the first seed from
/// (`cortex_testkit::seed::FIRST_SEED`).
const FIRST_SEED: &str = "CORTEX_TEST_FIRST_SEED";

/// The variable that fixes one seed (`cortex_testkit::seed::SEED`).
const SEED: &str = "CORTEX_TEST_SEED";

/// How many times each test is run when the count is not given.
const COUNT: u32 = 100;

/// What `cargo xtask stress` was asked for.
#[derive(Debug, PartialEq)]
struct Request {
    packages: Vec<String>,
    count: u32,
    first_seed: u64,
    /// The cores to pin the run to, and the busy loops beside it.
    squeeze: Squeeze,
    /// Arguments for nextest.
    rest: Vec<String>,
}

impl Request {
    fn parse(arguments: &[String]) -> Result<Self> {
        let mut request = Self {
            packages: Vec::new(),
            count: COUNT,
            first_seed: 0,
            squeeze: Squeeze::default(),
            rest: Vec::new(),
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            if request.squeeze.take(argument, &mut arguments)? {
                continue;
            }
            let mut value = |what: &str| {
                arguments
                    .next()
                    .ok_or_else(|| format!("`{argument}` needs {what}\n\n{USAGE}"))
            };
            match argument.as_str() {
                // What follows is for the test binaries, whatever it looks like.
                "--" => {
                    request.rest.push(argument.clone());
                    request.rest.extend(arguments.cloned());
                    break;
                }
                "-p" | "--package" => request.packages.push(value("a crate")?.clone()),
                joined if joined.starts_with("--package=") => {
                    request
                        .packages
                        .push(joined["--package=".len()..].to_owned());
                }
                joined if joined.starts_with("-p") && !joined.starts_with("--") => {
                    request.packages.push(joined[2..].to_owned());
                }
                "--count" => {
                    let text = value("a number")?;
                    request.count = text
                        .parse()
                        .ok()
                        .filter(|count| *count > 0)
                        .ok_or_else(|| format!("`--count {text}` is not a number above 0"))?;
                }
                "--first-seed" => {
                    let text = value("a number")?;
                    request.first_seed = text
                        .parse()
                        .map_err(|_| format!("`--first-seed {text}` is not a whole number"))?;
                }
                "--help" | "-h" => return Err(USAGE.to_owned()),
                "--stress-count" | "--stress-duration" | "--profile" | "-P" => {
                    return Err(format!(
                        "`{argument}` is set by this command. Use `--count`.\n\n{USAGE}"
                    ));
                }
                _ => request.rest.push(argument.clone()),
            }
        }
        if request.packages.is_empty() {
            return Err(format!(
                "say which crates to stress with `-p`. The whole workspace a hundred times \
                 over is not a question worth its time.\n\n{USAGE}"
            ));
        }
        Ok(request)
    }

    /// The arguments of `cargo` for this request.
    /// The build of the tests the run will use, with nothing run: the
    /// command's arguments with `--no-run` before what goes to nextest.
    fn build_command(&self) -> Vec<String> {
        let mut command: Vec<String> = ["nextest", "run", "--profile", "stress", "--no-run"]
            .map(str::to_owned)
            .to_vec();
        for package in &self.packages {
            command.extend(["-p".to_owned(), package.clone()]);
        }
        command.extend(self.rest.iter().take_while(|word| *word != "--").cloned());
        command
    }

    fn command(&self) -> Vec<String> {
        let mut command: Vec<String> = ["nextest", "run", "--profile", "stress", "--stress-count"]
            .map(str::to_owned)
            .to_vec();
        command.push(self.count.to_string());
        for package in &self.packages {
            command.extend(["-p".to_owned(), package.clone()]);
        }
        command.extend(self.rest.iter().cloned());
        command
    }
}

impl Request {
    /// cargo, as this request starts it: with the first seed, and without a
    /// seed that is fixed. One seed for every repeat would make every repeat
    /// the same run, and the run would say that nothing fails.
    fn cargo(&self, root: &std::path::Path) -> Command {
        let mut cargo = Command::new(workspace::cargo());
        cargo
            .args(self.command())
            .env(FIRST_SEED, self.first_seed.to_string())
            .env_remove(SEED)
            .current_dir(root);
        cargo
    }
}

/// `cargo xtask stress`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let request = Request::parse(arguments)?;
    checks::require_nextest()?;
    let root = workspace::root();
    let report = workspace::nextest_report(&root, "stress");
    // A report left by the run before must not be read as this run's.
    workspace::forget_report(&report)?;

    // Built before the squeeze, so that two pinned cores are spent on the
    // tests and not on the compiler.
    if request.squeeze != Squeeze::default() {
        let mut build = Command::new(workspace::cargo());
        build.args(request.build_command()).current_dir(&root);
        let built = build
            .status()
            .map_err(|error| format!("could not run cargo nextest: {error}"))?;
        if !built.success() {
            return Err("the tests did not build".to_owned());
        }
    }
    let _load = request.squeeze.start()?;
    println!(
        "$ {FIRST_SEED}={} cargo {}",
        request.first_seed,
        request.command().join(" ")
    );
    let status = request
        .cargo(&root)
        .status()
        .map_err(|error| format!("could not run cargo nextest: {error}"))?;

    let cases = junit::read(&report).map_err(|error| {
        format!("the stress run left no report to read ({error}). Did it build?")
    })?;
    let summary = summarise(&cases, request.count, request.first_seed);
    print!("{}", summary.text);
    conclude(&summary, status.success(), request.count)
}

/// Whether the run found nothing: no test failed on any repeat, nextest
/// agrees, and every repeat that was asked for is in the report.
fn conclude(summary: &Summary, nextest_passed: bool, count: u32) -> Result<()> {
    if summary.failing > 0 {
        return Err(format!("{} test(s) failed on some repeat", summary.failing));
    }
    if !nextest_passed {
        return Err(
            "nextest failed, and its report names no failed test. See its output above.".to_owned(),
        );
    }
    if summary.repeats_seen != count {
        return Err(format!(
            "the report has {} repeat(s) where {count} were asked for",
            summary.repeats_seen
        ));
    }
    Ok(())
}

/// What a stress run came to.
struct Summary {
    text: String,
    /// The tests that failed at least once.
    failing: usize,
    /// How many repeats the report holds.
    repeats_seen: u32,
}

/// How many seeds of a failing test are listed.
const SEEDS_SHOWN: usize = 12;

/// Reads a stress run's report: for each test that failed, on how many of
/// its repeats, with which seeds, and how to run one of them again.
fn summarise(cases: &[TestCase], count: u32, first_seed: u64) -> Summary {
    // For each test: how often it ran, and the repeats it failed on.
    let mut tests: BTreeMap<(String, String), (u32, Vec<u32>)> = BTreeMap::new();
    let mut repeats_seen = 0;
    for case in cases {
        // A report of a run that was not a stress run has one repeat.
        let repeat = case.repeat.unwrap_or(0);
        repeats_seen = repeats_seen.max(repeat + 1);
        let entry = tests
            .entry((case.binary.clone(), case.name.clone()))
            .or_default();
        entry.0 += 1;
        // A test that passed only when it was tried again failed this
        // repeat: nextest gives some tests a retry, and a test that needs
        // it some of the time is what this command is looking for.
        if case.failed || case.retried {
            entry.1.push(repeat);
        }
    }

    let mut text = String::new();
    let last_seed = first_seed.wrapping_add(u64::from(count.saturating_sub(1)));
    let _ = writeln!(
        text,
        "\n{} test(s), {count} time(s) each, with seeds {first_seed} to {last_seed}.",
        tests.len()
    );
    let failing: Vec<_> = tests
        .iter()
        .filter(|(_, (_, failed))| !failed.is_empty())
        .collect();
    if failing.is_empty() {
        let _ = writeln!(text, "Every test passed every time.");
    }
    for ((binary, name), (ran, failed)) in &failing {
        let mut failed = failed.clone();
        failed.sort_unstable();
        let seeds: Vec<String> = failed
            .iter()
            .take(SEEDS_SHOWN)
            .map(|repeat| first_seed.wrapping_add(u64::from(*repeat)).to_string())
            .collect();
        let more = failed.len().saturating_sub(SEEDS_SHOWN);
        let _ = writeln!(
            text,
            "\nFAILED {} of {ran} times: {binary} {name}",
            failed.len()
        );
        if failed.len() == usize::try_from(*ran).unwrap_or(usize::MAX) {
            let _ = writeln!(
                text,
                "  It fails every time: the order of events is not what it depends on."
            );
        }
        let _ = writeln!(
            text,
            "  seeds: {}{}",
            seeds.join(", "),
            if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            }
        );
        // nextest's binary id is `<package>` or `<package>::<target>`.
        let package = binary.split("::").next().unwrap_or(binary);
        let seed = first_seed.wrapping_add(u64::from(failed.first().copied().unwrap_or(0)));
        let _ = writeln!(
            text,
            "  again: {SEED}={seed} cargo xtask test -p {package} -E 'test(={name})'"
        );
    }
    Summary {
        text,
        failing: failing.len(),
        repeats_seen,
    }
}

#[cfg(test)]
mod tests {
    use super::{Request, summarise};
    use crate::junit;
    use crate::load::Squeeze;

    #[test]
    fn a_squeeze_is_this_commands_and_not_nextests() {
        let request = Request::parse(&strings(&[
            "-p",
            "cortex-testkit",
            "--load",
            "6",
            "--cores",
            "2",
            "server::",
        ]))
        .unwrap();
        assert_eq!(
            request.squeeze,
            Squeeze {
                load: 6,
                cores: Some(2)
            }
        );
        assert_eq!(
            request.command().join(" "),
            "nextest run --profile stress --stress-count 100 -p cortex-testkit server::"
        );
        // The build before the squeeze runs nothing, whatever goes after `--`.
        let request = Request::parse(&strings(&[
            "-p",
            "cortex-testkit",
            "--cores",
            "2",
            "server::",
            "--",
            "--exact",
            "a",
        ]))
        .unwrap();
        assert_eq!(
            request.build_command().join(" "),
            "nextest run --profile stress --no-run -p cortex-testkit server::"
        );
    }

    fn strings(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect()
    }

    #[test]
    fn a_request_is_read_and_what_it_does_not_know_goes_to_nextest() {
        let request = Request::parse(&strings(&[
            "-p",
            "cortex-agent",
            "--count",
            "300",
            "-p",
            "cortex-tools",
            "--first-seed",
            "1000",
            "denials::",
            "--run-ignored",
            "only",
        ]))
        .unwrap();

        assert_eq!(
            request,
            Request {
                packages: strings(&["cortex-agent", "cortex-tools"]),
                count: 300,
                first_seed: 1000,
                squeeze: Squeeze::default(),
                rest: strings(&["denials::", "--run-ignored", "only"]),
            }
        );
        assert_eq!(
            request.command().join(" "),
            "nextest run --profile stress --stress-count 300 -p cortex-agent -p cortex-tools \
             denials:: --run-ignored only"
        );
    }

    #[test]
    fn a_package_is_read_in_every_spelling_and_nothing_after_the_dashes_is() {
        let request = Request::parse(&strings(&[
            "--package=cortex-agent",
            "-pcortex-tools",
            "--",
            "-p",
            "--count",
        ]))
        .unwrap();

        assert_eq!(request.packages, strings(&["cortex-agent", "cortex-tools"]));
        assert_eq!(request.count, 100);
        assert_eq!(request.rest, strings(&["--", "-p", "--count"]));
    }

    #[test]
    fn the_run_is_given_the_first_seed_and_no_seed_that_is_fixed() {
        let request =
            Request::parse(&strings(&["-p", "cortex-agent", "--first-seed", "1000"])).unwrap();
        let cargo = request.cargo(std::path::Path::new("."));
        let variable = |name: &str| {
            cargo
                .get_envs()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };

        assert_eq!(
            variable("CORTEX_TEST_FIRST_SEED"),
            Some(Some("1000".to_owned()))
        );
        // Removed, whatever the caller's environment holds.
        assert_eq!(variable("CORTEX_TEST_SEED"), Some(None));
    }

    #[test]
    fn a_hundred_times_from_seed_zero_when_not_told() {
        let request = Request::parse(&strings(&["-p", "cortex-agent"])).unwrap();

        assert_eq!((request.count, request.first_seed), (100, 0));
    }

    #[test]
    fn a_request_that_makes_no_sense_is_refused() {
        for (arguments, expected) in [
            (&[][..], "say which crates to stress"),
            (&["--count", "5"], "say which crates to stress"),
            (&["-p"], "`-p` needs a crate"),
            (&["-p", "a", "--count", "0"], "is not a number above 0"),
            (&["-p", "a", "--count", "lots"], "is not a number above 0"),
            (&["-p", "a", "--first-seed", "-3"], "is not a whole number"),
            (
                &["-p", "a", "--stress-count", "5"],
                "is set by this command",
            ),
            (&["-p", "a", "--profile", "ci"], "is set by this command"),
        ] {
            let error = Request::parse(&strings(arguments)).unwrap_err();
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
    }

    /// Four repeats of three tests. One passes every time, one fails on
    /// repeats 1 and 3, one fails on all four.
    fn report() -> String {
        let mut xml = String::from("<testsuites name=\"nextest-run\" tests=\"12\">\n");
        for repeat in 0..4 {
            let failure = "<failure type=\"test failure with exit code 101\">no</failure>";
            let odd = if repeat % 2 == 1 { failure } else { "" };
            xml.push_str(&format!(
                "<testsuite name=\"cortex-agent::it@stress-{repeat}\" tests=\"3\">
    <testcase name=\"rounds::steady\" classname=\"cortex-agent::it\" time=\"0.01\"></testcase>
    <testcase name=\"denials::needs_one_order\" classname=\"cortex-agent::it\" time=\"0.01\">{odd}</testcase>
    <testcase name=\"denials::broken\" classname=\"cortex-agent::it\" time=\"0.01\">{failure}</testcase>
</testsuite>\n"
            ));
        }
        xml.push_str("</testsuites>\n");
        xml
    }

    #[test]
    fn a_test_that_fails_on_some_repeats_is_reported_with_the_seeds_that_fail_it() {
        let summary = summarise(&junit::parse(&report()), 4, 1000);

        assert_eq!(summary.failing, 2);
        assert_eq!(summary.repeats_seen, 4);
        assert_eq!(
            summary.text,
            "
3 test(s), 4 time(s) each, with seeds 1000 to 1003.

FAILED 4 of 4 times: cortex-agent::it denials::broken
  It fails every time: the order of events is not what it depends on.
  seeds: 1000, 1001, 1002, 1003
  again: CORTEX_TEST_SEED=1000 cargo xtask test -p cortex-agent -E 'test(=denials::broken)'

FAILED 2 of 4 times: cortex-agent::it denials::needs_one_order
  seeds: 1001, 1003
  again: CORTEX_TEST_SEED=1001 cargo xtask test -p cortex-agent -E 'test(=denials::needs_one_order)'
"
        );
    }

    #[test]
    fn a_run_in_which_nothing_failed_says_so() {
        let report = report().replace(
            "<failure type=\"test failure with exit code 101\">no</failure>",
            "",
        );

        let summary = summarise(&junit::parse(&report), 4, 0);

        assert_eq!(summary.failing, 0);
        assert_eq!(
            summary.text,
            "\n3 test(s), 4 time(s) each, with seeds 0 to 3.\nEvery test passed every time.\n"
        );
    }

    #[test]
    fn a_test_that_passed_only_on_its_retry_failed_that_repeat() {
        // Repeat 1 failed and then passed when nextest tried it again.
        let report = "<testsuite name=\"a@stress-0\"><testcase name=\"timing_sensitive::t\" \
                      classname=\"a\" time=\"0.1\"/></testsuite><testsuite \
                      name=\"a@stress-1\"><testcase name=\"timing_sensitive::t\" \
                      classname=\"a\" time=\"0.1\"><flakyFailure>no</flakyFailure></testcase>\
                      </testsuite>";

        let summary = summarise(&junit::parse(report), 2, 0);

        assert_eq!(summary.failing, 1);
        assert!(
            summary
                .text
                .contains("FAILED 1 of 2 times: a timing_sensitive::t\n  seeds: 1\n"),
            "{}",
            summary.text
        );
    }

    #[test]
    fn a_run_found_nothing_only_if_nextest_agrees_and_every_repeat_is_there() {
        let clean = summarise(
            &junit::parse(&report().replace(
                "<failure type=\"test failure with exit code 101\">no</failure>",
                "",
            )),
            4,
            0,
        );
        let failing = summarise(&junit::parse(&report()), 4, 0);

        assert_eq!(super::conclude(&clean, true, 4), Ok(()));
        let error = super::conclude(&failing, false, 4).unwrap_err();
        assert!(error.contains("2 test(s) failed on some repeat"), "{error}");
        // nextest failed and no test is named: a build that broke, say.
        let error = super::conclude(&clean, false, 4).unwrap_err();
        assert!(error.contains("names no failed test"), "{error}");
        // The run was cut short.
        let error = super::conclude(&clean, true, 5).unwrap_err();
        assert!(
            error.contains("4 repeat(s) where 5 were asked for"),
            "{error}"
        );
    }

    #[test]
    fn a_long_list_of_seeds_is_cut_and_counted() {
        let mut xml = String::new();
        for repeat in 0..20 {
            xml.push_str(&format!(
                "<testsuite name=\"a@stress-{repeat}\"><testcase name=\"t\" classname=\"a\" \
                 time=\"0.1\"><failure/></testcase></testsuite>"
            ));
        }

        let summary = summarise(&junit::parse(&xml), 20, 0);

        assert!(
            summary
                .text
                .contains("seeds: 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, and 8 more\n"),
            "{}",
            summary.text
        );
        assert!(
            summary
                .text
                .contains("again: CORTEX_TEST_SEED=0 cargo xtask test -p a -E 'test(=t)'"),
            "{}",
            summary.text
        );
    }
}
