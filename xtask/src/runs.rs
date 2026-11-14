//! CI runs on GitHub, read through `gh`: a run's JUnit reports and its
//! jobs' times. `cargo xtask ci --tiers --compare` reads reports from here,
//! and `cargo xtask ci-times` prints a run's tables in the form the steps'
//! `ci.md` files use (`docs/testing-and-ci.md`, "A CI run's test times").
//!
//! Nothing here writes to GitHub. `gh` must be installed and signed in; it
//! asks for nothing but reads.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::Result;
use crate::junit::{self, TestCase};
use crate::tiers::{self, Os, Tier};
use crate::workspace;

/// Where a report comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    /// A JUnit report on disk.
    File(PathBuf),
    /// A run of the `ci` workflow, by its id.
    Run(u64),
    /// The main branch's latest successful run of the `ci` workflow.
    Main,
}

impl Source {
    /// `main`, a run's id (digits), or a path.
    pub(crate) fn parse(text: &str) -> Self {
        if text == "main" {
            Source::Main
        } else if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
            text.parse()
                .map_or_else(|_| Source::File(text.into()), Source::Run)
        } else {
            Source::File(text.into())
        }
    }
}

/// Runs `gh` with `arguments` and returns what it printed.
fn gh(arguments: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .args(arguments)
        .current_dir(workspace::root())
        .output()
        .map_err(|error| format!("could not run gh (is it installed?): {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`gh {}` failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| {
        format!(
            "`gh {}` printed bytes that are not text",
            arguments.join(" ")
        )
    })
}

/// How many of the main branch's latest successful runs are looked at for
/// one that kept the reports asked for.
const MAIN_RUNS_LOOKED_AT: usize = 20;

/// What became of a run's artifact of one name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// It is there to download.
    Yes,
    /// The run made none: its tests did not run (a change of documentation
    /// alone, a job that was cancelled or failed before the upload).
    Never,
    /// It was made and has expired.
    Expired,
}

/// `name`'s fate among a run's artifacts, each a name and whether it has
/// expired, as GitHub lists them.
pub(crate) fn kept(artifacts: &[(String, bool)], name: &str) -> Kept {
    match artifacts.iter().find(|(artifact, _)| artifact == name) {
        None => Kept::Never,
        Some((_, true)) => Kept::Expired,
        Some((_, false)) => Kept::Yes,
    }
}

/// A run's artifacts, as `name expired` lines from GitHub's API, read.
pub(crate) fn parse_artifacts(listed: &str) -> Result<Vec<(String, bool)>> {
    listed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match line.trim().rsplit_once(' ') {
            Some((name, "true")) => Ok((name.to_owned(), true)),
            Some((name, "false")) => Ok((name.to_owned(), false)),
            _ => Err(format!(
                "an artifact line that is not `name expired`: `{line}`"
            )),
        })
        .collect()
}

/// A run's artifacts, asked of GitHub. A failure here is an error, never
/// "no artifacts": the network or the sign-in is not the run.
fn artifacts(run: u64) -> Result<Vec<(String, bool)>> {
    parse_artifacts(&gh(&[
        "api",
        &format!("repos/{{owner}}/{{repo}}/actions/runs/{run}/artifacts?per_page=100"),
        "--jq",
        ".artifacts[] | \"\\(.name) \\(.expired)\"",
    ])?)
}

/// The first of `runs` (newest first, each with its artifacts) that kept
/// every artifact of `needs`.
pub(crate) fn first_that_kept(runs: &[(u64, Vec<(String, bool)>)], needs: &[&str]) -> Option<u64> {
    runs.iter()
        .find(|(_, artifacts)| needs.iter().all(|name| kept(artifacts, name) == Kept::Yes))
        .map(|(run, _)| *run)
}

/// The main branch's latest successful `ci` run that kept the reports of
/// `needs`: a run of a documentation change alone is successful and keeps
/// none, and is passed over. It is printed, so that a reader knows which
/// run `main` was.
pub(crate) fn latest_main_run(needs: &[&str]) -> Result<u64> {
    let limit = MAIN_RUNS_LOOKED_AT.to_string();
    let listed = gh(&[
        "run",
        "list",
        "--workflow",
        "ci",
        "--branch",
        "main",
        "--status",
        "success",
        "--limit",
        &limit,
        "--json",
        "databaseId,headSha",
    ])?;
    let value: Value =
        serde_json::from_str(&listed).map_err(|error| format!("gh run list: {error}"))?;
    let listed: Vec<(u64, String)> = value
        .as_array()
        .map(|runs| {
            runs.iter()
                .filter_map(|run| {
                    Some((
                        run.get("databaseId")?.as_u64()?,
                        run.get("headSha")?.as_str()?.chars().take(7).collect(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        return Err("the main branch has no successful run of the `ci` workflow".to_owned());
    }
    // One at a time, newest first: most of the time the first will do.
    for (run, sha) in &listed {
        let artifacts = artifacts(*run)?;
        if first_that_kept(&[(*run, artifacts)], needs).is_some() {
            println!(
                "main: run {run} (at {sha}), the main branch's latest successful `ci` run that \
                 kept {}",
                needs.join(" and ")
            );
            return Ok(*run);
        }
    }
    Err(format!(
        "none of the main branch's last {} successful `ci` runs kept {}",
        listed.len(),
        needs.join(" and ")
    ))
}

/// The run a source names, if it names one. `needs` is what a run of `main`
/// must have kept.
pub(crate) fn run_of(source: &Source, needs: &[&str]) -> Result<Option<u64>> {
    match source {
        Source::File(_) => Ok(None),
        Source::Run(id) => Ok(Some(*id)),
        Source::Main => latest_main_run(needs).map(Some),
    }
}

/// The JUnit report of a run's test job on `os`: downloaded once, into a
/// directory of its own under `target/xtask-runs/`, and read from there
/// afterwards. A run's artifacts do not change.
///
/// `None` only when the run made no such report. An expired report, a
/// download that fails, and a question GitHub does not answer are errors.
pub(crate) fn junit_of(run: u64, os: Os) -> Result<Option<PathBuf>> {
    let name = artifact_name(os);
    let directory = download_root().join(run.to_string()).join(name);
    let report = directory.join("junit.xml");
    if report.is_file() {
        return Ok(Some(report));
    }
    match kept(&artifacts(run)?, name) {
        Kept::Yes => {}
        Kept::Never => return Ok(None),
        Kept::Expired => {
            return Err(format!(
                "run {run}'s `{name}` has expired: a run's reports are kept for 14 days"
            ));
        }
    }
    // A partial download from an earlier attempt is not trusted.
    if directory.exists() {
        fs::remove_dir_all(&directory)
            .map_err(|error| format!("could not clear {}: {error}", directory.display()))?;
    }
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not make {}: {error}", directory.display()))?;
    let run_text = run.to_string();
    let target = directory.to_string_lossy().into_owned();
    gh(&[
        "run", "download", &run_text, "--name", name, "--dir", &target,
    ])
    .map_err(|error| format!("run {run}'s `{name}` is there and could not be fetched: {error}"))?;
    if report.is_file() {
        Ok(Some(report))
    } else {
        Err(format!(
            "run {run}'s artifact `{name}` has no junit.xml (see {})",
            directory.display()
        ))
    }
}

fn artifact_name(os: Os) -> &'static str {
    match os {
        Os::Linux => "junit-linux",
        Os::Windows => "junit-windows",
    }
}

/// A report and its label, from wherever `source` says.
pub(crate) fn report(source: &Source, os: Os) -> Result<(String, Vec<TestCase>)> {
    match run_of(source, &[artifact_name(os)])? {
        None => {
            let Source::File(path) = source else {
                unreachable!("only a file names no run")
            };
            Ok((path.display().to_string(), junit::read(path)?))
        }
        Some(run) => {
            let label = match source {
                Source::Main => format!("run {run}, the main branch's latest successful"),
                _ => format!("run {run}"),
            };
            let report = junit_of(run, os)?.ok_or_else(|| {
                format!(
                    "run {run} kept no `{}`: its tests did not run",
                    artifact_name(os)
                )
            })?;
            Ok((label, junit::read(&report)?))
        }
    }
}

/// What `ci-times` reads of a run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunInfo {
    pub(crate) id: u64,
    pub(crate) sha: String,
    pub(crate) branch: String,
    pub(crate) event: String,
    pub(crate) date: String,
    /// Each job's name, and how long it took in whole seconds when it ran
    /// to an end.
    pub(crate) jobs: Vec<(String, Option<u64>)>,
}

/// Reads `gh run view <id> --json databaseId,headSha,headBranch,event,createdAt,jobs`.
pub(crate) fn parse_run(json: &str) -> Result<RunInfo> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| format!("gh run view: {error}"))?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut jobs = Vec::new();
    for job in value
        .get("jobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = job
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let at = |key: &str| job.get(key).and_then(Value::as_str).and_then(seconds_of);
        let took = match (at("startedAt"), at("completedAt")) {
            (Some(start), Some(end)) if end >= start => Some(end - start),
            _ => None,
        };
        jobs.push((name, took));
    }
    Ok(RunInfo {
        id: value.get("databaseId").and_then(Value::as_u64).unwrap_or(0),
        sha: text("headSha"),
        branch: text("headBranch"),
        event: text("event"),
        date: text("createdAt").chars().take(10).collect(),
        jobs,
    })
}

/// Seconds since 1970 of a time as GitHub writes it, `2026-11-13T10:02:03Z`.
/// `0001-01-01T00:00:00Z`, which GitHub writes for a job that never
/// started, is none.
fn seconds_of(time: &str) -> Option<u64> {
    let (date, clock) = time.strip_suffix('Z')?.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut clock = clock.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    if year < 1970 {
        return None;
    }
    // Days from the civil date (the proleptic Gregorian calendar).
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let of_era = shifted - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

fn run_info(run: u64) -> Result<RunInfo> {
    let id = run.to_string();
    parse_run(&gh(&[
        "run",
        "view",
        &id,
        "--json",
        "databaseId,headSha,headBranch,event,createdAt,jobs",
    ])?)
}

/// `cargo xtask ci-times <run> [--against <run>|main]`.
pub(crate) fn ci_times(arguments: &[String]) -> Result<()> {
    const USAGE: &str = "usage: cargo xtask ci-times <RUN ID> [--against <RUN ID>|main]";
    let (run, against) = match arguments {
        [run] => (run, None),
        [run, flag, against] if flag == "--against" => (run, Some(against)),
        _ => return Err(USAGE.to_owned()),
    };
    let run = match Source::parse(run) {
        Source::Run(id) => id,
        _ => return Err(format!("`{run}` is not a run's id\n{USAGE}")),
    };
    let against = match against.map(|text| Source::parse(text)) {
        None => None,
        Some(Source::File(path)) => {
            return Err(format!(
                "`{}` is not a run's id or `main`\n{USAGE}",
                path.display()
            ));
        }
        Some(source) => run_of(
            &source,
            &[artifact_name(Os::Linux), artifact_name(Os::Windows)],
        )?,
    };
    let mut runs = Vec::new();
    for id in against.into_iter().chain([run]) {
        let info = run_info(id)?;
        let mut reports = Vec::new();
        for os in [Os::Linux, Os::Windows] {
            // A run whose tests did not run (a documentation change, a
            // cancelled job) has no report for that OS, and its column says
            // so. Anything else that keeps a report from being read is an
            // error: a table without the column would not say why.
            let cases = match junit_of(id, os)? {
                Some(path) => Some(junit::read(&path)?),
                None => {
                    println!(
                        "run {id} kept no `{}`: its tests did not run",
                        artifact_name(os)
                    );
                    None
                }
            };
            reports.push(cases);
        }
        runs.push(Measured {
            info,
            linux: reports[0].take(),
            windows: reports[1].take(),
        });
    }
    print!("{}", tables(&runs));
    Ok(())
}

/// A run, and its test jobs' reports where they exist.
pub(crate) struct Measured {
    pub(crate) info: RunInfo,
    pub(crate) linux: Option<Vec<TestCase>>,
    pub(crate) windows: Option<Vec<TestCase>>,
}

/// The two tables of a step's `ci.md`: each job's time, and each tier's
/// time above the start against its budget, one column for each run (and,
/// for the tiers, each OS).
pub(crate) fn tables(runs: &[Measured]) -> String {
    let short = |sha: &str| sha.chars().take(7).collect::<String>();
    let mut text = String::new();
    for measured in runs {
        let info = &measured.info;
        let _ = writeln!(
            text,
            "Run {} at `{}`: {} on `{}`, {}.",
            info.id,
            short(&info.sha),
            info.event,
            info.branch,
            info.date
        );
    }
    text.push('\n');

    let column = |info: &RunInfo| format!("Run {}, `{}`", info.id, short(&info.sha));
    let _ = write!(text, "| Job |");
    for measured in runs {
        let _ = write!(text, " {} |", column(&measured.info));
    }
    let _ = writeln!(text, "\n|---|{}", "---|".repeat(runs.len()));
    let mut names: Vec<&str> = Vec::new();
    for measured in runs {
        for (name, _) in &measured.info.jobs {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
    }
    // Linux's jobs, then Windows', as the steps' pages list them.
    names.sort_by_key(|name| (name.contains("windows"), *name));
    for name in names {
        let _ = write!(text, "| `{name}` |");
        for measured in runs {
            let took = measured
                .info
                .jobs
                .iter()
                .find(|(job, _)| job == name)
                .and_then(|(_, took)| *took);
            match took {
                Some(seconds) => {
                    let _ = write!(text, " {seconds} s |");
                }
                None => text.push_str(" — |"),
            }
        }
        text.push('\n');
    }

    let mut columns: Vec<(Os, &Measured, &Vec<TestCase>)> = Vec::new();
    for os in [Os::Linux, Os::Windows] {
        for measured in runs {
            let cases = match os {
                Os::Linux => measured.linux.as_ref(),
                Os::Windows => measured.windows.as_ref(),
            };
            if let Some(cases) = cases {
                columns.push((os, measured, cases));
            }
        }
    }
    if columns.is_empty() {
        text.push_str("\nNo test job of these runs kept a JUnit report.\n");
        return text;
    }
    text.push_str("\n| Time above process start, against the budget |");
    let starts: Vec<f64> = columns
        .iter()
        .map(|(_, _, cases)| tiers::start_of(cases).0)
        .collect();
    for ((os, measured, _), start) in columns.iter().zip(&starts) {
        let os = match os {
            Os::Linux => "Linux",
            Os::Windows => "Windows",
        };
        let _ = write!(
            text,
            " {os}, run {} ({:.0} ms) |",
            measured.info.id,
            1000.0 * start
        );
    }
    let _ = writeln!(text, "\n|---|{}", "---|".repeat(columns.len()));
    for tier in Tier::ALL {
        let budget = match tier {
            Tier::System => "60 s on Linux, 90 s on Windows".to_owned(),
            _ => format!("{:.0} s", tier.budget(Os::Linux)),
        };
        let (number, name) = tier.name().split_once(", ").unwrap_or(("", tier.name()));
        let _ = write!(text, "| Tier {number}, {name} ({budget}) |");
        for ((_, _, cases), start) in columns.iter().zip(&starts) {
            let _ = write!(
                text,
                " {:.1} s |",
                tiers::total(cases, tier, *start).above_start
            );
        }
        text.push('\n');
    }
    text
}

/// Where `ci-times` and `--compare` keep what they download: a directory
/// of its own for each run and artifact, under the build directory.
fn download_root() -> PathBuf {
    workspace::root().join("target").join("xtask-runs")
}

#[cfg(test)]
mod artifact_tests {
    use super::{Kept, first_that_kept, kept, parse_artifacts};

    #[test]
    fn a_report_is_absent_only_when_the_run_made_none() {
        let artifacts = parse_artifacts("junit-linux false\njunit-windows true\n").unwrap();
        assert_eq!(kept(&artifacts, "junit-linux"), Kept::Yes);
        assert_eq!(kept(&artifacts, "junit-windows"), Kept::Expired);
        assert_eq!(kept(&artifacts, "junit-other"), Kept::Never);
        assert_eq!(parse_artifacts("").unwrap(), []);
        assert!(parse_artifacts("junit-linux maybe\n").is_err());
        // A name with a space in it keeps it.
        assert_eq!(
            parse_artifacts("a b false").unwrap(),
            [("a b".to_owned(), false)]
        );
    }

    #[test]
    fn main_is_the_newest_run_that_kept_every_report_asked_for() {
        let named = |names: &[(&str, bool)]| {
            names
                .iter()
                .map(|(name, expired)| ((*name).to_owned(), *expired))
                .collect::<Vec<_>>()
        };
        let runs = vec![
            // A change of documentation alone: no report.
            (30, named(&[])),
            (29, named(&[("junit-linux", false)])),
            (
                28,
                named(&[("junit-linux", false), ("junit-windows", true)]),
            ),
            (
                27,
                named(&[("junit-linux", false), ("junit-windows", false)]),
            ),
        ];
        assert_eq!(first_that_kept(&runs, &["junit-linux"]), Some(29));
        assert_eq!(
            first_that_kept(&runs, &["junit-linux", "junit-windows"]),
            Some(27)
        );
        assert_eq!(first_that_kept(&runs[..3], &["junit-windows"]), None);
    }
}

#[cfg(test)]
mod tests {
    use super::{Measured, RunInfo, Source, download_root, parse_run, seconds_of, tables};
    use crate::junit;

    #[test]
    fn a_source_is_main_a_run_or_a_file() {
        assert_eq!(Source::parse("main"), Source::Main);
        assert_eq!(Source::parse("19283746501"), Source::Run(19_283_746_501));
        assert_eq!(
            Source::parse("junit/junit.xml"),
            Source::File("junit/junit.xml".into())
        );
        // Too many digits for a run's id: a file of that name.
        assert_eq!(
            Source::parse("99999999999999999999999"),
            Source::File("99999999999999999999999".into())
        );
        assert!(download_root().ends_with("target/xtask-runs"));
    }

    #[test]
    fn github_times_are_read() {
        assert_eq!(seconds_of("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(seconds_of("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(seconds_of("2026-11-13T10:02:03Z"), Some(1_794_564_123));
        // A job that never started.
        assert_eq!(seconds_of("0001-01-01T00:00:00Z"), None);
        assert_eq!(seconds_of("yesterday"), None);
    }

    /// What `gh run view --json …` printed for a run, cut down.
    const RUN: &str = r#"{"createdAt":"2026-11-13T09:58:00Z","databaseId":4242,"event":"pull_request","headBranch":"step-x","headSha":"0123456789abcdef","jobs":[
        {"name":"lint-linux","startedAt":"2026-11-13T10:00:00Z","completedAt":"2026-11-13T10:00:48Z","conclusion":"success"},
        {"name":"test-linux","startedAt":"2026-11-13T10:00:00Z","completedAt":"2026-11-13T10:00:56Z","conclusion":"success"},
        {"name":"test-windows","startedAt":"2026-11-13T10:00:05Z","completedAt":"2026-11-13T10:02:23Z","conclusion":"success"},
        {"name":"lint-windows","startedAt":"0001-01-01T00:00:00Z","completedAt":"0001-01-01T00:00:00Z","conclusion":"cancelled"}]}"#;

    #[test]
    fn a_run_is_read_from_what_gh_prints() {
        let info = parse_run(RUN).unwrap();
        assert_eq!(
            info,
            RunInfo {
                id: 4242,
                sha: "0123456789abcdef".to_owned(),
                branch: "step-x".to_owned(),
                event: "pull_request".to_owned(),
                date: "2026-11-13".to_owned(),
                jobs: vec![
                    ("lint-linux".to_owned(), Some(48)),
                    ("test-linux".to_owned(), Some(56)),
                    ("test-windows".to_owned(), Some(138)),
                    ("lint-windows".to_owned(), None),
                ],
            }
        );
        assert!(parse_run("not json").is_err());
    }

    #[test]
    fn the_tables_are_those_of_a_steps_ci_page() {
        let main = RunInfo {
            id: 4000,
            sha: "fedcba9876543210".to_owned(),
            branch: "main".to_owned(),
            event: "push".to_owned(),
            date: "2026-11-12".to_owned(),
            jobs: vec![
                ("lint-linux".to_owned(), Some(52)),
                ("test-linux".to_owned(), Some(68)),
            ],
        };
        let runs = [
            Measured {
                info: main,
                linux: Some(junit::parse(include_str!(
                    "../fixtures/tiers/compare-main.xml"
                ))),
                windows: None,
            },
            Measured {
                info: parse_run(RUN).unwrap(),
                linux: Some(junit::parse(include_str!(
                    "../fixtures/tiers/compare-branch.xml"
                ))),
                windows: Some(junit::parse(include_str!("../fixtures/tiers/junit.xml"))),
            },
        ];
        assert_eq!(
            tables(&runs),
            "Run 4000 at `fedcba9`: push on `main`, 2026-11-12.
Run 4242 at `0123456`: pull_request on `step-x`, 2026-11-13.

| Job | Run 4000, `fedcba9` | Run 4242, `0123456` |
|---|---|---|
| `lint-linux` | 52 s | 48 s |
| `test-linux` | 68 s | 56 s |
| `lint-windows` | — | — |
| `test-windows` | — | 138 s |

| Time above process start, against the budget | Linux, run 4000 (10 ms) | Linux, run 4242 (20 ms) | Windows, run 4242 (200 ms) |
|---|---|---|---|
| Tier 1, unit (20 s) | 1.0 s | 1.2 s | 5.0 s |
| Tier 2, integration (40 s) | 1.5 s | 3.5 s | 9.2 s |
| Tier 3, system (60 s on Linux, 90 s on Windows) | 2.0 s | 2.0 s | 14.2 s |
"
        );
    }
}
