//! The check and test commands. The blocking CI jobs run `check` and
//! `ci test`, so the commands here are the definition of what CI runs
//! (`docs/testing-and-ci.md`).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::Result;
use crate::mutate;
use crate::rules;
use crate::runs::{self, Source};
use crate::schema;
use crate::steps::{self, Step};
use crate::workspace;
use crate::{compare, tiers};

/// The tier 0 steps, in the order they run: cheapest first.
const CHECK_STEPS: [&str; 7] = [
    "fmt",
    "rules",
    "breaks",
    "generated",
    "actionlint",
    "clippy",
    "deny",
];

fn check_step(name: &str) -> Option<Step> {
    Some(match name {
        "fmt" => Step::cargo("fmt", &["fmt", "--all", "--check"]),
        "rules" => Step::function("rules", rules),
        // Every break in `mutations/` still finds the code it changes
        // (docs/testing-and-ci.md, "Breaking the code on purpose"). It reads
        // a few files and runs nothing.
        "breaks" => Step::function("breaks", mutate::check_breaks_apply),
        // The checked-in protocol schema and TypeScript declarations are what
        // the types generate (docs/architecture/11-rpc-protocol.md).
        "generated" => Step::function("generated", schema::check),
        "clippy" => Step::cargo(
            "clippy",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        ),
        // Advisories are not here: a new advisory against an unchanged
        // lockfile must not block unrelated pull requests. They run nightly.
        "deny" => Step::cargo("deny", &["deny", "check", "licenses", "bans", "sources"]),
        "actionlint" => Step::function("actionlint", actionlint),
        _ => return None,
    })
}

/// `cargo xtask check [STEP...]`
pub(crate) fn check(arguments: &[String]) -> Result<()> {
    let names: Vec<&str> = if arguments.is_empty() {
        CHECK_STEPS.to_vec()
    } else {
        arguments.iter().map(String::as_str).collect()
    };
    let mut selected = Vec::new();
    for name in names {
        selected.push(check_step(name).ok_or_else(|| {
            format!(
                "unknown check `{name}`. The checks are: {}",
                CHECK_STEPS.join(", ")
            )
        })?);
    }
    steps::run("Static checks", selected)
}

/// `cargo xtask rules`
pub(crate) fn rules() -> Result<()> {
    let violations = rules::check_repository(&workspace::root())?;
    if violations.is_empty() {
        println!("The repository follows its rules.");
        return Ok(());
    }
    for violation in &violations {
        println!("{violation}\n");
    }
    Err(format!("{} repo-rule violation(s)", violations.len()))
}

/// `cargo xtask links`: the part of the repo rules that is about the docs,
/// by itself. It reads text files and asks cargo nothing, so it answers
/// before a page that was just edited is out of mind.
pub(crate) fn links() -> Result<()> {
    let root = workspace::root();
    let files = workspace::tracked_files(&root)?;
    let names = workspace::repository_files(&root)?;
    let violations = rules::check_docs(&root, &files, &names)?;
    if violations.is_empty() {
        println!(
            "Every link inside the repository leads somewhere, and the step records are in their places."
        );
        return Ok(());
    }
    for violation in &violations {
        println!("{violation}\n");
    }
    Err(format!(
        "{} broken link(s) or misplaced record(s)",
        violations.len()
    ))
}

/// `cargo xtask fmt`
pub(crate) fn fmt() -> Result<()> {
    steps::cargo(&["fmt", "--all"])
}

/// `cargo xtask test [ARGS...]`
///
/// The whole workspace, unless the arguments name packages: `test -p
/// cortex-agent` builds and runs that crate's tests and no other's.
pub(crate) fn test(arguments: &[String]) -> Result<()> {
    require_nextest()?;
    let command = test_command(arguments);
    workspace::say_implied(&workspace::implied_features(&workspace::packages_named(
        arguments,
    )));
    steps::cargo(&command)
}

/// The arguments of `cargo` for `cargo xtask test <arguments>`.
fn test_command(arguments: &[String]) -> Vec<String> {
    let mut command = vec!["nextest".to_owned(), "run".to_owned()];
    if !selects_packages(arguments) {
        command.push("--workspace".to_owned());
    }
    command.extend(workspace::implied_features(&workspace::packages_named(
        arguments,
    )));
    command.extend(arguments.iter().cloned());
    command
}

/// Whether arguments for `cargo nextest run` already say which packages to
/// build and test. To cargo, `--workspace` beside `-p` means the whole
/// workspace, so it is added only when nothing was chosen.
pub(crate) fn selects_packages(arguments: &[String]) -> bool {
    arguments
        .iter()
        // What follows `--` goes to the test binaries.
        .take_while(|argument| *argument != "--")
        .any(|argument| {
            matches!(
                argument.as_str(),
                "-p" | "--package" | "--workspace" | "--all"
            ) || argument.starts_with("--package=")
                // `-pcortex-agent`. No other option of nextest starts `-p`.
                || (argument.starts_with("-p") && !argument.starts_with("--"))
        })
}

/// Fails with a pointer to the installer when cargo-nextest is missing,
/// instead of cargo's "no such command".
pub(crate) fn require_nextest() -> Result<()> {
    let found = Command::new(workspace::cargo())
        .args(["nextest", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if found {
        return Ok(());
    }
    Err(
        "cargo-nextest is not installed. See https://nexte.st. The version CI uses is in \
         docs/dependencies.md."
            .to_owned(),
    )
}

/// The blocking test job: tiers 1 to 3, in nextest's `ci` profile, built with
/// cargo's `ci` profile (third-party crates unoptimised, so that a cold cache
/// still fits the time budget). Building and running are separate steps so
/// that their times are reported separately.
fn ci_test_steps() -> Vec<Step> {
    let run = ci_run(&[]);
    let mut build = run.clone();
    build.push("--no-run".to_owned());
    vec![
        // The last run's report is not this run's. If the build fails, the
        // tier report then says there is nothing to read.
        Step::closure("build tests", move || {
            workspace::forget_report(&tiers::ci_report_path())?;
            steps::cargo(&build)
        }),
        Step::cargo_owned("run tests", run),
        // Each test has its own limit in .config/nextest.toml. This checks
        // what the tests of each tier took together.
        Step::function("tier budgets", tiers::check_ci_report),
    ]
}

/// `cargo nextest run` as the blocking test job runs it, for the whole
/// workspace or for the packages named.
pub(crate) fn ci_run(packages: &[String]) -> Vec<String> {
    let mut run = vec!["nextest".to_owned(), "run".to_owned()];
    if packages.is_empty() {
        run.push("--workspace".to_owned());
    }
    for package in packages {
        run.extend(["-p".to_owned(), package.clone()]);
    }
    run.extend(workspace::implied_features(packages));
    run.extend(["--locked", "--cargo-profile", "ci", "--profile", "ci"].map(str::to_owned));
    run
}

/// The version of actionlint CI installs (`.github/workflows/ci.yml`,
/// `try.yml`; `docs/dependencies.md`, "Tools").
const ACTIONLINT: &str = "actionlint";

/// The workflows, read by actionlint: their expressions, the inputs and
/// outputs they name, the runners and the types of their keys, which the
/// repo rules do not read. Its checks of the scripts by shellcheck and
/// pyflakes are off: those depend on what the machine has installed.
///
/// CI installs it. Without it at a desk the step says so and passes, as
/// the owner guard does without its terms; in CI a missing tool fails.
fn actionlint() -> Result<()> {
    let root = workspace::root();
    let mut workflows: Vec<String> = std::fs::read_dir(root.join(".github").join("workflows"))
        .map_err(|error| format!("could not list the workflows: {error}"))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".yml") || name.ends_with(".yaml"))
        .map(|name| format!(".github/workflows/{name}"))
        .collect();
    workflows.sort();
    let found = Command::new(ACTIONLINT)
        .args(["-shellcheck=", "-pyflakes=", "-no-color"])
        .args(&workflows)
        .current_dir(&root)
        .status();
    match found {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err("actionlint found a problem in a workflow (above)".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::env::var_os("GITHUB_ACTIONS").is_some() {
                Err(
                    "actionlint is not installed: the workflow installs it before this step"
                        .to_owned(),
                )
            } else {
                println!(
                    "actionlint is not installed here, so the workflows were not linted. CI \
                     lints them; the version is in docs/dependencies.md, \"Tools\"."
                );
                Ok(())
            }
        }
        Err(error) => Err(format!("could not run actionlint: {error}")),
    }
}

/// `cargo xtask ci [lint|test]`, and `cargo xtask ci --tiers ...`
pub(crate) fn ci(arguments: &[String]) -> Result<()> {
    if arguments.first().is_some_and(|first| first == "--tiers") {
        return ci_tiers(&arguments[1..]);
    }
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["lint"] => check(&[]),
        ["test"] => {
            require_nextest()?;
            steps::run("Tests", ci_test_steps())
        }
        [] => {
            require_nextest()?;
            let mut all: Vec<Step> = CHECK_STEPS
                .iter()
                .filter_map(|name| check_step(name))
                .collect();
            all.extend(ci_test_steps());
            steps::run("Everything that blocks a pull request", all)
        }
        _ => Err(CI_USAGE.to_owned()),
    }
}

const CI_USAGE: &str = "usage: cargo xtask ci [lint|test]
       cargo xtask ci --tiers [-p CRATE]... [--top N] [-- NEXTEST ARGUMENT...]
       cargo xtask ci --tiers --from JUNIT.XML [--os linux|windows] [--top N]
       cargo xtask ci --tiers --compare NEW [--against OLD] [--os linux|windows] [--top N]
                              NEW and OLD: a JUnit report, a run's id, or `main` (OLD's default)";

/// How many of each tier's slowest tests `ci --tiers` lists when it is not
/// told.
const TIERS_TOP: usize = 10;

/// What `cargo xtask ci --tiers` was asked for.
#[derive(Debug, PartialEq)]
struct TiersRequest {
    /// The packages to build and test. Empty: the workspace.
    packages: Vec<String>,
    /// How many of each tier's slowest tests to list.
    top: usize,
    /// A report to read in place of running anything.
    from: Option<PathBuf>,
    /// The OS the report is from, which decides tier 3's budget.
    os: tiers::Os,
    /// A report to compare with `against`, in place of running anything.
    compare: Option<Source>,
    /// What `compare` is compared with: the main branch when not given.
    against: Option<Source>,
    /// What follows `--`: more arguments for nextest, a filter for example.
    rest: Vec<String>,
}

impl TiersRequest {
    fn parse(arguments: &[String]) -> Result<Self> {
        let mut request = Self {
            packages: Vec::new(),
            top: TIERS_TOP,
            from: None,
            os: tiers::Os::here(),
            compare: None,
            against: None,
            rest: Vec::new(),
        };
        let mut os_given = false;
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let mut value = |what: &str| {
                rest.next()
                    .ok_or_else(|| format!("`{argument}` needs {what}\n{CI_USAGE}"))
            };
            match argument.as_str() {
                "--" => {
                    request.rest = rest.cloned().collect();
                    break;
                }
                "-p" | "--package" => request.packages.push(value("a crate")?.clone()),
                "--top" => {
                    let text = value("a number")?;
                    request.top = text
                        .parse()
                        .map_err(|_| format!("`--top {text}` is not a number"))?;
                }
                "--from" => request.from = Some(PathBuf::from(value("a JUnit report")?)),
                "--compare" => {
                    request.compare = Some(Source::parse(value("a report, a run or `main`")?));
                }
                "--against" => {
                    request.against = Some(Source::parse(value("a report, a run or `main`")?));
                }
                "--os" => {
                    os_given = true;
                    request.os = match value("`linux` or `windows`")?.as_str() {
                        "linux" => tiers::Os::Linux,
                        "windows" => tiers::Os::Windows,
                        other => {
                            return Err(format!("`--os {other}`: say `linux` or `windows`"));
                        }
                    };
                }
                other => return Err(format!("unknown argument `{other}`\n{CI_USAGE}")),
            }
        }
        if request.against.is_some() && request.compare.is_none() {
            return Err("`--against` says what `--compare` is compared with".to_owned());
        }
        if request.compare.is_some() && request.from.is_some() {
            return Err("`--compare` and `--from` both read a report: give one".to_owned());
        }
        if request.compare.is_some() {
            request.against.get_or_insert(Source::Main);
        }
        let reads = request.from.is_some() || request.compare.is_some();
        if reads && !(request.packages.is_empty() && request.rest.is_empty()) {
            return Err(
                "`--from` and `--compare` read reports and run nothing, so `-p` and arguments \
                 for nextest have no meaning with them"
                    .to_owned(),
            );
        }
        if os_given && !reads {
            return Err(
                "`--os` says where a report given with `--from` was made. A run made here is \
                 measured against this machine's budgets."
                    .to_owned(),
            );
        }
        Ok(request)
    }
}

/// `cargo xtask ci --tiers`: what each tier's tests take together, against
/// its budget, and which tests that time is made of.
///
/// It builds and runs as the blocking test job does, in cargo's `ci` profile,
/// where third-party crates are not optimised. A desk's own profile
/// optimises them, and a test that spends its time in one (a JSON reader, a
/// regular-expression engine) is several times slower in CI than it looks at
/// a desk. With `--from` it runs nothing and reports on a JUnit report that
/// is already there: the one a CI run uploads, for example.
fn ci_tiers(arguments: &[String]) -> Result<()> {
    let request = TiersRequest::parse(arguments)?;
    let (top, os) = (request.top, request.os);
    if let Some(report) = request.from {
        return tiers::check_report(&report, os, top);
    }
    if let (Some(new), Some(old)) = (&request.compare, &request.against) {
        let (label, cases) = runs::report(new, os)?;
        let new = compare::Side { label, cases };
        let (label, cases) = runs::report(old, os)?;
        let old = compare::Side { label, cases };
        print!("{}", compare::compare(&new, &old, os, top));
        return Ok(());
    }
    require_nextest()?;
    let mut run = ci_run(&request.packages);
    run.extend(request.rest.iter().cloned());
    let mut build = ci_run(&request.packages);
    build.push("--no-run".to_owned());
    let title = if request.packages.is_empty() && request.rest.is_empty() {
        "Test time by tier, in CI's profile".to_owned()
    } else {
        let mut chosen = request.packages.clone();
        chosen.extend(request.rest.iter().cloned());
        format!(
            "Test time by tier, in CI's profile: {} only (the budgets are the whole workspace's)",
            chosen.join(" ")
        )
    };
    steps::run(
        &title,
        vec![
            Step::closure("build tests", move || {
                workspace::forget_report(&tiers::ci_report_path())?;
                steps::cargo(&build)
            }),
            Step::cargo_owned("run tests", run),
            Step::closure("tier budgets", move || {
                tiers::check_report(&tiers::ci_report_path(), os, top)
            }),
        ],
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CHECK_STEPS, TiersRequest, check, check_step, ci, ci_run, selects_packages, test_command,
    };
    use crate::runs::Source;
    use crate::tiers::Os;

    fn strings(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect()
    }

    #[test]
    fn every_listed_check_exists() {
        for name in CHECK_STEPS {
            assert!(check_step(name).is_some(), "{name}");
        }
    }

    #[test]
    fn an_unknown_check_is_refused_before_anything_runs() {
        let error = check(&["clipy".to_owned()]).unwrap_err();
        assert!(error.contains("unknown check `clipy`"));
        assert!(error.contains(&CHECK_STEPS.join(", ")));
    }

    #[test]
    fn naming_a_package_keeps_the_rest_of_the_workspace_out() {
        for chosen in [
            &["-p", "cortex-agent"][..],
            &["--package", "cortex-agent"],
            &["--package=cortex-agent"],
            &["-pcortex-agent"],
            &["--profile", "fast", "-p", "cortex-agent", "denials"],
            &["--workspace", "--exclude", "cortex-cli"],
        ] {
            assert!(selects_packages(&strings(chosen)), "{chosen:?}");
        }
    }

    #[test]
    fn a_run_of_one_crate_is_not_a_run_of_the_workspace() {
        assert_eq!(
            super::test_command(&strings(&["-p", "cortex-agent", "denials::"])).join(" "),
            "nextest run -p cortex-agent denials::"
        );
        assert_eq!(
            super::test_command(&strings(&["--profile", "fast"])).join(" "),
            "nextest run --workspace --profile fast"
        );
        assert_eq!(
            super::test_command(&[]).join(" "),
            "nextest run --workspace"
        );
    }

    #[test]
    fn without_a_package_the_whole_workspace_is_tested() {
        for open in [
            &[][..],
            &["--profile", "fast"],
            &["denials::"],
            &["--exclude", "cortex-cli"],
            &["--no-fail-fast", "-E", "test(/sys_/)"],
            // Arguments for the test binaries are not nextest's.
            &["--", "-p"],
            &["--", "--package"],
        ] {
            assert!(!selects_packages(&strings(open)), "{open:?}");
        }
    }

    /// The `backends` trap: a build of cortex-sandbox alone that tests
    /// nothing it confines.
    #[test]
    fn the_sandbox_alone_is_built_with_its_backends_on_linux() {
        let test = test_command(&strings(&["-p", "cortex-sandbox", "confined::"])).join(" ");
        let ci = ci_run(&strings(&["cortex-sandbox"])).join(" ");
        let other = test_command(&strings(&["-p", "cortex-agent"])).join(" ");
        let spelled = test_command(&strings(&["--package=cortex-sandbox"])).join(" ");
        if cfg!(target_os = "linux") {
            assert_eq!(
                test,
                "nextest run --features cortex-sandbox/backends -p cortex-sandbox confined::"
            );
            assert!(ci.contains("--features cortex-sandbox/backends"), "{ci}");
            assert!(
                spelled.contains("--features cortex-sandbox/backends"),
                "{spelled}"
            );
        } else {
            assert!(!test.contains("--features"), "{test}");
        }
        assert!(!other.contains("--features"), "{other}");
        // The whole workspace turns it on through cortex-core.
        assert!(!test_command(&[]).join(" ").contains("--features"));
    }

    #[test]
    fn the_ci_run_is_the_workspace_or_the_packages_named() {
        assert_eq!(
            ci_run(&[]).join(" "),
            "nextest run --workspace --locked --cargo-profile ci --profile ci"
        );
        assert_eq!(
            ci_run(&strings(&["cortex-agent", "cortex-tools"])).join(" "),
            "nextest run -p cortex-agent -p cortex-tools --locked --cargo-profile ci --profile ci"
        );
    }

    #[test]
    fn a_tiers_request_is_read() {
        assert_eq!(
            TiersRequest::parse(&[]).unwrap(),
            TiersRequest {
                packages: vec![],
                top: 10,
                from: None,
                os: Os::here(),
                compare: None,
                against: None,
                rest: vec![],
            }
        );
        assert_eq!(
            TiersRequest::parse(&strings(&[
                "-p",
                "cortex-llm",
                "--top",
                "3",
                "--",
                "replay::",
                "--top",
            ]))
            .unwrap(),
            TiersRequest {
                packages: strings(&["cortex-llm"]),
                top: 3,
                from: None,
                os: Os::here(),
                compare: None,
                against: None,
                // After `--` nothing is this command's, whatever it looks like.
                rest: strings(&["replay::", "--top"]),
            }
        );
        assert_eq!(
            TiersRequest::parse(&strings(&["--from", "junit.xml", "--os", "windows"])).unwrap(),
            TiersRequest {
                packages: vec![],
                top: 10,
                from: Some(PathBuf::from("junit.xml")),
                os: Os::Windows,
                compare: None,
                against: None,
                rest: vec![],
            }
        );
    }

    #[test]
    fn a_comparison_is_against_the_main_branch_unless_told() {
        let request =
            TiersRequest::parse(&strings(&["--compare", "123", "--os", "windows"])).unwrap();
        assert_eq!(request.compare, Some(Source::Run(123)));
        assert_eq!(request.against, Some(Source::Main));
        assert_eq!(request.os, Os::Windows);
        let request =
            TiersRequest::parse(&strings(&["--compare", "a.xml", "--against", "b.xml"])).unwrap();
        assert_eq!(request.compare, Some(Source::File("a.xml".into())));
        assert_eq!(request.against, Some(Source::File("b.xml".into())));
    }

    #[test]
    fn a_tiers_request_that_makes_no_sense_is_refused() {
        for (arguments, expected) in [
            (&["--top"][..], "needs a number"),
            (&["--top", "many"], "is not a number"),
            (&["--os", "macos", "--from", "x"], "`linux` or `windows`"),
            (&["--os", "windows"], "`--os` says where a report"),
            (&["--from", "x", "-p", "cortex-llm"], "run nothing"),
            (&["--from", "x", "--", "replay::"], "run nothing"),
            (&["--compare", "x", "-p", "cortex-llm"], "run nothing"),
            (
                &["--against", "main"],
                "says what `--compare` is compared with",
            ),
            (&["--compare", "1", "--from", "x"], "give one"),
            (&["--compare"], "needs a report, a run or `main`"),
            (&["--fast"], "unknown argument `--fast`"),
        ] {
            let error = TiersRequest::parse(&strings(arguments)).unwrap_err();
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
    }

    #[test]
    fn an_unknown_ci_argument_is_refused_with_the_usage() {
        let error = ci(&strings(&["tests"])).unwrap_err();
        assert!(error.contains("ci --tiers"), "{error}");
    }
}
