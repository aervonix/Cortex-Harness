//! Reading the JUnit report nextest writes.
//!
//! Three commands read it: the tier budgets (`tiers`), which need each
//! test's time; `stress`, which needs to know which repeat of a test failed;
//! and `mutate`, which needs the names of the tests a break made fail. A
//! report is the one place nextest says these things in a form meant for a
//! program: its terminal output pads and reorders, and reading it has
//! misled a mutation run before.
//!
//! It reads only what nextest writes: attributes in double quotes, and the
//! `<failure>` and `<error>` children of a `<testcase>`.

use std::fs;
use std::path::Path;

use crate::Result;

/// One `<testcase>` of a report.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TestCase {
    /// nextest's binary id: `cortex-base`, `cortex-agent::it`, `cortex-cli::system`.
    pub(crate) binary: String,
    pub(crate) name: String,
    pub(crate) seconds: f64,
    /// The test failed, timed out or could not be run. A test that passed
    /// on a retry is not failed.
    pub(crate) failed: bool,
    /// The test failed and then passed when it was tried again. The run
    /// counts it as passed; a search for tests that fail some of the time
    /// counts it as one.
    pub(crate) retried: bool,
    /// The repeat this case is from, counted from 0, when the report is of
    /// a `--stress-count` run. nextest names the suite `<binary>@stress-<n>`.
    pub(crate) repeat: Option<u32>,
}

impl TestCase {
    /// `<binary> <name>`: how nextest writes a test, and how a break names
    /// the tests that catch it.
    pub(crate) fn id(&self) -> String {
        format!("{} {}", self.binary, self.name)
    }
}

/// Reads a report from a file. A report with no test in it is an error: a
/// run that listed nothing checked nothing.
pub(crate) fn read(report: &Path) -> Result<Vec<TestCase>> {
    let xml = fs::read_to_string(report)
        .map_err(|error| format!("could not read {}: {error}", report.display()))?;
    let cases = parse(&xml);
    if cases.is_empty() {
        return Err(format!("{} lists no tests", report.display()));
    }
    Ok(cases)
}

/// Reads the `<testcase>` elements of a report.
pub(crate) fn parse(xml: &str) -> Vec<TestCase> {
    let mut cases = Vec::new();
    for suite in xml.split("<testsuite ").skip(1) {
        let suite_tag = suite.split('>').next().unwrap_or(suite);
        let repeat = attribute(suite_tag, "name").and_then(|name| {
            let (_, number) = name.rsplit_once("@stress-")?;
            number.parse().ok()
        });
        for element in suite.split("<testcase ").skip(1) {
            let tag = element.split('>').next().unwrap_or(element);
            // `<testcase ... />` has no children. Otherwise they run to the
            // closing tag.
            let children = if tag.ends_with('/') {
                ""
            } else {
                let after_tag = &element[tag.len()..];
                after_tag.split("</testcase>").next().unwrap_or(after_tag)
            };
            // nextest always writes a time. A case without one is still a
            // case: dropping it would drop a failure with it.
            let seconds = attribute(tag, "time")
                .and_then(|time| time.parse().ok())
                .unwrap_or(0.0);
            if let (Some(name), Some(binary)) =
                (attribute(tag, "name"), attribute(tag, "classname"))
            {
                cases.push(TestCase {
                    binary: unescape(binary),
                    name: unescape(name),
                    seconds,
                    // Text is escaped in a report, so a `<` begins an
                    // element. A test that passed on a retry has
                    // `<flakyFailure>` children, which neither of these
                    // matches.
                    failed: children.contains("<failure") || children.contains("<error"),
                    retried: children.contains("<flakyFailure") || children.contains("<flakyError"),
                    repeat,
                });
            }
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

/// Undoes the five escapes XML has for attribute values.
fn unescape(value: &str) -> String {
    if !value.contains('&') {
        return value.to_owned();
    }
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{TestCase, parse};

    /// A report as nextest writes it, with the attributes in both orders, a
    /// test with output, a failure, a timeout, and a test that passed on
    /// its retry.
    pub(crate) const REPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="7" failures="2" errors="0" time="0.9">
    <testsuite name="cortex-base" tests="1" disabled="0" errors="0" failures="0">
        <testcase name="text::tests::wraps_at_the_width" classname="cortex-base" timestamp="2026-11-12T13:25:40.000+00:00" time="0.004">
        </testcase>
    </testsuite>
    <testsuite name="cortex-agent::it" tests="3" disabled="0" errors="0" failures="2">
        <testcase classname="cortex-agent::it" name="loop_::retries_after_a_stall" time="0.250"/>
        <testcase name="loop_::stops_at_the_refused_call" classname="cortex-agent::it" time="0.031">
            <failure message="thread &apos;loop_::stops_at_the_refused_call&apos; panicked" type="test failure with exit code 101">assertion `left == right` failed
  left: [&quot;a&quot;]
 right: [&quot;a&quot;, &quot;b&quot;]</failure>
            <system-out>running 1 test</system-out>
        </testcase>
        <testcase name="loop_::never_ends" classname="cortex-agent::it" time="10.002">
            <failure type="test timeout"/>
        </testcase>
    </testsuite>
    <testsuite name="cortex-cli::system" tests="3" disabled="0" errors="0" failures="0">
        <testcase name="sys_version_is_byte_identical_under_both_names" classname="cortex-cli::system" time="0.5">
            <system-out>cortex 0.0.0</system-out>
        </testcase>
        <testcase name="rules::tests::sys_the_repository_follows_its_own_rules" classname="xtask::bin/xtask" time="0.125">
        </testcase>
        <testcase name="timing_sensitive::sys_passes_the_second_time" classname="cortex-cli::system" time="0.2">
            <flakyFailure message="first try" type="test failure with exit code 101">no</flakyFailure>
        </testcase>
    </testsuite>
</testsuites>
"#;

    pub(crate) fn case(binary: &str, name: &str, seconds: f64) -> TestCase {
        TestCase {
            binary: binary.to_owned(),
            name: name.to_owned(),
            seconds,
            failed: false,
            retried: false,
            repeat: None,
        }
    }

    fn failed(binary: &str, name: &str, seconds: f64) -> TestCase {
        TestCase {
            failed: true,
            ..case(binary, name, seconds)
        }
    }

    #[test]
    fn the_report_is_read_whatever_the_attribute_order() {
        assert_eq!(
            parse(REPORT),
            [
                case("cortex-base", "text::tests::wraps_at_the_width", 0.004),
                case("cortex-agent::it", "loop_::retries_after_a_stall", 0.25),
                failed(
                    "cortex-agent::it",
                    "loop_::stops_at_the_refused_call",
                    0.031
                ),
                failed("cortex-agent::it", "loop_::never_ends", 10.002),
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
                TestCase {
                    retried: true,
                    ..case(
                        "cortex-cli::system",
                        "timing_sensitive::sys_passes_the_second_time",
                        0.2
                    )
                },
            ]
        );
    }

    #[test]
    fn a_failure_belongs_to_its_own_test_and_not_to_the_one_before() {
        let cases = parse(REPORT);
        let failures: Vec<&str> = cases
            .iter()
            .filter(|case| case.failed)
            .map(|case| case.name.as_str())
            .collect();

        assert_eq!(
            failures,
            ["loop_::stops_at_the_refused_call", "loop_::never_ends"]
        );
    }

    #[test]
    fn a_test_that_passed_on_its_retry_is_not_a_failure() {
        let cases = parse(REPORT);
        let flaky = cases
            .iter()
            .find(|case| case.name.ends_with("passes_the_second_time"))
            .unwrap();

        assert!(!flaky.failed);
    }

    #[test]
    fn a_stress_run_says_which_repeat_each_case_is_from() {
        let report = r#"<testsuites name="nextest-run" tests="4" failures="1">
    <testsuite name="probe@stress-0" tests="2" failures="0">
        <testcase name="tests::steady" classname="probe" time="0.004"/>
        <testcase name="tests::odd_fails" classname="probe" time="0.004"/>
    </testsuite>
    <testsuite name="probe@stress-1" tests="2" failures="1">
        <testcase name="tests::steady" classname="probe" time="0.006"/>
        <testcase name="tests::odd_fails" classname="probe" time="0.006">
            <failure message="fails on 1" type="test failure with exit code 101">fails on 1</failure>
        </testcase>
    </testsuite>
</testsuites>"#;

        let cases = parse(report);

        let seen: Vec<(Option<u32>, &str, bool)> = cases
            .iter()
            .map(|case| (case.repeat, case.name.as_str(), case.failed))
            .collect();
        assert_eq!(
            seen,
            [
                (Some(0), "tests::steady", false),
                (Some(0), "tests::odd_fails", false),
                (Some(1), "tests::steady", false),
                (Some(1), "tests::odd_fails", true),
            ]
        );
    }

    #[test]
    fn an_escaped_name_is_read_as_it_was_written() {
        let report = r#"<testsuite name="a"><testcase name="tests::a&lt;b&gt;&amp;c" classname="a" time="1"/></testsuite>"#;

        assert_eq!(parse(report)[0].name, "tests::a<b>&c");
        assert_eq!(parse(report)[0].id(), "a tests::a<b>&c");
    }

    #[test]
    fn a_case_without_a_time_is_still_a_case_and_its_failure_still_a_failure() {
        let report = r#"<testsuite name="a"><testcase name="tests::x" classname="a"><failure/></testcase><testcase name="tests::y" classname="a" time="soon"/></testsuite>"#;

        assert_eq!(
            parse(report),
            [failed("a", "tests::x", 0.0), case("a", "tests::y", 0.0)]
        );
    }

    #[test]
    fn a_test_that_could_not_be_run_is_a_failure() {
        let report = r#"<testsuite name="a"><testcase name="tests::x" classname="a" time="0.1"><error type="could not start">no such file</error></testcase></testsuite>"#;

        assert_eq!(parse(report), [failed("a", "tests::x", 0.1)]);
    }

    #[test]
    fn a_test_that_needed_its_retry_is_marked() {
        let cases = parse(REPORT);
        let retried: Vec<&str> = cases
            .iter()
            .filter(|case| case.retried)
            .map(|case| case.name.as_str())
            .collect();

        assert_eq!(retried, ["timing_sensitive::sys_passes_the_second_time"]);
    }

    #[test]
    fn an_escaped_ampersand_is_not_read_as_the_start_of_another_escape() {
        // The name `a&lt;b`, which is not the name `a<b`.
        let report = r#"<testsuite name="a"><testcase name="tests::a&amp;lt;b" classname="a" time="1"/></testsuite>"#;

        assert_eq!(parse(report)[0].name, "tests::a&lt;b");
    }
}
