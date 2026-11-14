//! `cargo xtask ab`: two trees' tests on one machine, run in turn, and
//! their tiers compared (`docs/testing-and-ci.md`, "Two trees on one
//! runner").
//!
//! Two CI runs land on two runners, and hosted Windows runners differ in
//! speed by a factor of two: a change's cost to a tier cannot be told apart
//! from the runner it drew. This builds the tests of both trees once each,
//! each in a build directory of its own, then runs them in turn on the same
//! machine, `count` rounds of one run each, the first in the order base,
//! head and the next head, base, so that whatever drifts during the job
//! (a warmer disk, a busier neighbour) falls on both alike. Each test's time
//! is the median of its runs, and the two sides are compared as
//! `ci --tiers --compare` compares two CI runs. Each round's tier totals are
//! printed too: their spread on one side is the noise a difference has to
//! stand out of.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::compare::{self, Side};
use crate::junit::{self, TestCase};
use crate::tiers::{self, Os, Tier};
use crate::workspace;

const USAGE: &str = "\
usage: cargo xtask ab --base DIR [--label NAME] [--count N] [--top N] [-p CRATE]...
                      [-- NEXTEST ARGUMENT...]

Builds the tests of this tree (the head) and of the tree at DIR (the base)
once each, as the blocking test job does, each in a build directory of its
own; runs them in turn on this machine, N rounds (5 when not given) in
alternating order; and compares each test's median time, by tier, crate and
test, as `ci --tiers --compare` does. NAME is what the base is called in the
report (its directory when not given). `cargo xtask try` runs it on a hosted
runner with the input `against`.";

/// Rounds when not told: enough for a median, few enough for a Windows job.
const ROUNDS: usize = 5;

/// The most rounds a run may ask for.
const MOST_ROUNDS: usize = 50;

/// What `cargo xtask ab` was asked for.
#[derive(Debug, PartialEq)]
pub(crate) struct Request {
    pub(crate) base: PathBuf,
    pub(crate) label: Option<String>,
    pub(crate) rounds: usize,
    pub(crate) top: usize,
    pub(crate) packages: Vec<String>,
    /// For nextest, after `--`.
    pub(crate) rest: Vec<String>,
}

impl Request {
    pub(crate) fn parse(arguments: &[String]) -> Result<Self> {
        let mut base = None;
        let mut request = Request {
            base: PathBuf::new(),
            label: None,
            rounds: ROUNDS,
            top: 10,
            packages: Vec::new(),
            rest: Vec::new(),
        };
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            let mut value = |what: &str| {
                words
                    .next()
                    .ok_or_else(|| format!("`{word}` needs {what}\n\n{USAGE}"))
            };
            match word.as_str() {
                "--" => {
                    request.rest = words.cloned().collect();
                    break;
                }
                "--base" => base = Some(PathBuf::from(value("a directory")?)),
                "--label" => request.label = Some(value("a name")?.clone()),
                "--count" => {
                    let text = value("a number")?;
                    request.rounds = text
                        .parse()
                        .ok()
                        .filter(|rounds| (2..=MOST_ROUNDS).contains(rounds))
                        .ok_or_else(|| {
                            format!("`--count {text}`: a number of rounds from 2 to {MOST_ROUNDS}")
                        })?;
                }
                "--top" => {
                    let text = value("a number")?;
                    request.top = text
                        .parse()
                        .map_err(|_| format!("`--top {text}` is not a number"))?;
                }
                "-p" | "--package" => request.packages.push(value("a crate")?.clone()),
                "--help" | "-h" => return Err(USAGE.to_owned()),
                other => return Err(format!("unknown argument `{other}`\n\n{USAGE}")),
            }
        }
        request.base = base.ok_or_else(|| format!("say which tree is the base\n\n{USAGE}"))?;
        Ok(request)
    }
}

/// Which side runs first in round `round`, counted from 0: base, head,
/// then head, base, and so on.
pub(crate) fn order(round: usize) -> [Which; 2] {
    if round.is_multiple_of(2) {
        [Which::Base, Which::Head]
    } else {
        [Which::Head, Which::Base]
    }
}

/// One of the two trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    Base,
    Head,
}

/// Each test's median time over the runs of one side. A test that is not in
/// every run keeps the median of the runs it is in.
pub(crate) fn medians(runs: &[Vec<TestCase>]) -> Vec<TestCase> {
    let mut times: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    for run in runs {
        for case in run {
            times
                .entry((case.binary.clone(), case.name.clone()))
                .or_default()
                .push(case.seconds);
        }
    }
    times
        .into_iter()
        .map(|((binary, name), mut seconds)| {
            seconds.sort_by(f64::total_cmp);
            let middle = seconds.len() / 2;
            let median = if seconds.len() % 2 == 1 {
                seconds[middle]
            } else {
                f64::midpoint(seconds[middle - 1], seconds[middle])
            };
            TestCase {
                binary,
                name,
                seconds: median,
                failed: false,
                retried: false,
                repeat: None,
            }
        })
        .collect()
}

/// Each round's tier times above the start, for both sides: the spread of
/// one side is the noise.
pub(crate) fn rounds_table(base: &[Vec<TestCase>], head: &[Vec<TestCase>]) -> String {
    let mut text = String::from(
        "Each round, each tier's time above the start of its processes, in seconds:\n\n\
         round  side   start      unit  integration     system\n",
    );
    for (round, (base, head)) in base.iter().zip(head).enumerate() {
        for (side, cases) in [("base", base), ("head", head)] {
            let (start, _) = tiers::start_of(cases);
            let [unit, integration, system] =
                Tier::ALL.map(|tier| tiers::total(cases, tier, start).above_start);
            let _ = writeln!(
                text,
                "{:>5}  {side}  {:>5.0} ms  {unit:>8.1}  {integration:>11.1}  {system:>9.1}",
                round + 1,
                start * 1000.0
            );
        }
    }
    text
}

/// The trees' build directories: each its own, whatever the caller's
/// environment says, so that building one never rebuilds the other.
fn target_of(tree: &Path) -> PathBuf {
    tree.join("target")
}

/// `cargo nextest run` in `tree`, as the blocking test job runs it.
fn nextest(tree: &Path, request: &Request, build_only: bool) -> Command {
    let mut command = Command::new(workspace::cargo());
    command
        .current_dir(tree)
        .env("CARGO_TARGET_DIR", target_of(tree))
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .args(crate::checks::ci_run(&request.packages));
    if build_only {
        command.arg("--no-run");
    } else {
        command.args(&request.rest);
    }
    command
}

fn run_in(tree: &Path, mut command: Command, what: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| format!("could not run cargo in {}: {error}", tree.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed in {} ({status})", tree.display()))
    }
}

/// `cargo xtask ab …`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let request = Request::parse(arguments)?;
    crate::checks::require_nextest()?;
    let head = workspace::root();
    let base = std::path::absolute(&request.base)
        .map_err(|error| format!("{}: {error}", request.base.display()))?;
    if !base.join("Cargo.toml").is_file() {
        return Err(format!(
            "{} is not a checkout of the workspace",
            base.display()
        ));
    }
    if base == head {
        return Err("the base is this tree: give another checkout".to_owned());
    }
    let label = request
        .label
        .clone()
        .unwrap_or_else(|| base.display().to_string());
    let tree = |which: Which| if which == Which::Base { &base } else { &head };

    for which in [Which::Base, Which::Head] {
        println!("== building the {} ==", name(which, &label));
        run_in(
            tree(which),
            nextest(tree(which), &request, true),
            "the build",
        )?;
    }
    let mut runs: [Vec<Vec<TestCase>>; 2] = [Vec::new(), Vec::new()];
    for round in 0..request.rounds {
        for which in order(round) {
            println!(
                "== round {} of {}: the {} ==",
                round + 1,
                request.rounds,
                name(which, &label)
            );
            let at = tree(which);
            let report = workspace::nextest_report(at, "ci");
            workspace::forget_report(&report)?;
            run_in(at, nextest(at, &request, false), "the tests")?;
            let cases = junit::read(&report)?;
            runs[which as usize].push(cases);
        }
    }
    let [base_runs, head_runs] = runs;
    println!("\n{}", rounds_table(&base_runs, &head_runs));
    let base_side = Side {
        label: format!("base, {label} (the median of {} runs)", base_runs.len()),
        cases: medians(&base_runs),
    };
    let head_side = Side {
        label: format!("head, this tree (the median of {} runs)", head_runs.len()),
        cases: medians(&head_runs),
    };
    let compared = compare::compare(&head_side, &base_side, Os::here(), request.top);
    println!("{compared}");
    crate::steps::append_job_summary(&format!(
        "### Two trees on one runner\n\n```text\n{}\n{compared}\n```\n",
        rounds_table(&base_runs, &head_runs)
    ));
    Ok(())
}

fn name(which: Which, label: &str) -> String {
    match which {
        Which::Base => format!("base ({label})"),
        Which::Head => "head (this tree)".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Request, Which, medians, order, rounds_table};
    use crate::junit::TestCase;

    fn case(binary: &str, name: &str, seconds: f64) -> TestCase {
        TestCase {
            binary: binary.to_owned(),
            name: name.to_owned(),
            seconds,
            failed: false,
            retried: false,
            repeat: None,
        }
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_request_is_read_and_what_makes_no_sense_is_refused() {
        assert_eq!(
            Request::parse(&strings(&[
                "--base",
                "../main",
                "--count",
                "8",
                "-p",
                "cortex-base",
                "--label",
                "main",
                "--",
                "-E",
                "test(/x/)"
            ]))
            .unwrap(),
            Request {
                base: "../main".into(),
                label: Some("main".to_owned()),
                rounds: 8,
                top: 10,
                packages: strings(&["cortex-base"]),
                rest: strings(&["-E", "test(/x/)"]),
            }
        );
        assert_eq!(
            Request::parse(&strings(&["--base", "b"])).unwrap().rounds,
            5
        );
        for (words, expected) in [
            (&["--count", "3"][..], "say which tree is the base"),
            (&["--base", "b", "--count", "1"], "from 2 to 50"),
            (&["--base", "b", "--count", "51"], "from 2 to 50"),
            (&["--base"], "needs a directory"),
            (&["--base", "b", "--os", "linux"], "unknown argument"),
        ] {
            let error = Request::parse(&strings(words)).unwrap_err();
            assert!(error.contains(expected), "{words:?}: {error}");
        }
    }

    #[test]
    fn the_order_alternates_so_drift_falls_on_both_sides() {
        assert_eq!(order(0), [Which::Base, Which::Head]);
        assert_eq!(order(1), [Which::Head, Which::Base]);
        assert_eq!(order(2), [Which::Base, Which::Head]);
    }

    #[test]
    fn each_test_takes_its_median_and_one_slow_run_moves_nothing() {
        let runs = vec![
            vec![case("a", "t", 0.010), case("a", "u", 0.5)],
            vec![case("a", "t", 0.012), case("a", "u", 9.0)],
            vec![case("a", "t", 0.011), case("a", "u", 0.6)],
            vec![case("a", "t", 0.013)],
        ];
        let medians = medians(&runs);
        assert_eq!(medians.len(), 2);
        assert!((medians[0].seconds - 0.0115).abs() < 1e-9, "{medians:?}");
        // The run of 9 s is one of three: the median is the middle one.
        assert!((medians[1].seconds - 0.6).abs() < 1e-9, "{medians:?}");
    }

    #[test]
    fn each_round_is_a_line_for_each_side() {
        let base = vec![vec![case("cortex-base", "t", 0.5)]; 2];
        let head = vec![vec![case("cortex-base", "t", 0.75)]; 2];
        let table = rounds_table(&base, &head);
        let lines: Vec<&str> = table
            .lines()
            .filter(|line| line.contains(" base ") || line.contains(" head "))
            .collect();
        assert_eq!(lines.len(), 4, "{table}");
        assert!(lines[0].trim_start().starts_with("1  base"), "{table}");
        assert!(lines[3].trim_start().starts_with("2  head"), "{table}");
    }
}
