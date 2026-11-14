//! `cargo xtask try`: what the `try` workflow runs on a hosted runner, for
//! a branch with no pull request (`docs/testing-and-ci.md`, "A run on the
//! runners without a pull request").
//!
//! The workflow hands over its inputs as they were typed, each as one
//! argument, and this turns them into one of a few commands. Doing it here
//! and not in the workflow's script keeps the rules in one place, on both
//! OSes, under unit tests; and a typed input is only ever split into words
//! and checked, never handed to a shell.

use crate::Result;

const USAGE: &str = "\
usage: cargo xtask try --command 'COMMAND [ARGUMENTS]' [--profile ci|release|dist]
                       [--count N] [--load N] [--cores K] [--filter 'EXPR; EXPR']
                       [--against REF]

COMMAND is one of:
  test [-p CRATE]... [FILTER]...   the tests, as the blocking job builds and runs
                                   them, with the tier report; with --count,
                                   `stress` with that many repeats
  stress -p CRATE... [ARGS]...     the tests many times over (--count repeats)
  startup                          start-up of `cortex` built in the profile
                                   (release or dist), --count runs
  perf [ARGS]...                   the performance budgets (step 20)
  ci [ARGS]...                     any form of `cargo xtask ci`

A word of `test` or `stress` that is not `-p CRATE` goes to nextest: a test's
name, or an option such as `--run-ignored only`. --filter takes nextest filter
expressions, separated by `;`, each passed as one `-E` argument, spaces and
all: `test(/shell::/) & !test(=slow); package(cortex-agent)`. nextest runs a
test that any of them matches.

--against REF makes `test` an A/B run: the workflow checks REF out beside the
branch, and `cargo xtask ab` builds both once and runs them in turn, --count
rounds (5 when not given), on one runner, then compares them by tier, crate
and test.

An empty value is the same as leaving the flag out.";

/// The commands a `try` run may run.
const COMMANDS: [&str; 5] = ["test", "stress", "startup", "perf", "ci"];

/// The workflow's inputs.
#[derive(Debug, Default, PartialEq)]
struct Inputs {
    words: Vec<String>,
    profile: String,
    count: Option<u32>,
    load: Option<usize>,
    cores: Option<usize>,
    /// nextest filter expressions, each one `-E` argument.
    filters: Vec<String>,
    /// The ref an A/B run compares the branch with.
    against: Option<String>,
}

/// Where the workflow puts the ref of an A/B run: beside the branch's
/// checkout, not in it, so that the branch's `.cargo/config.toml` does not
/// apply to its build.
pub(crate) const AB_BASE: &str = "../ab-base";

/// Whether `expression` may be passed to nextest as a filter: what
/// [`plain`] allows, and the spaces, brackets and marks of a filter or a
/// regular expression. No quote, `$`, `;`, back-tick or control character.
fn filter_text(expression: &str) -> bool {
    !expression.trim().is_empty()
        && expression.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || b" _-:./=,()|&!~*+@#^[]{}?\\".contains(&byte)
        })
}

/// Whether `word` may reach a command line: letters, digits and the marks
/// a crate's name, a test's name, a path or a nextest filter needs. No
/// quote, no `$`, no `;`, no space: a word is passed as one argument and
/// never to a shell, and this keeps it that way should either change.
fn plain(word: &str) -> bool {
    !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-:./=,()|&!~*+@#".contains(&byte))
}

fn number<T>(flag: &str, text: &str, most: T) -> Result<Option<T>>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display + Default,
{
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<T>() {
        Ok(value) if value > T::default() && value <= most => Ok(Some(value)),
        _ => Err(format!("`{flag} {text}`: a number from 1 to {most}")),
    }
}

impl Inputs {
    fn parse(arguments: &[String]) -> Result<Self> {
        let mut inputs = Inputs {
            profile: "ci".to_owned(),
            ..Inputs::default()
        };
        let mut rest = arguments.iter();
        while let Some(flag) = rest.next() {
            if flag == "--help" || flag == "-h" {
                return Err(USAGE.to_owned());
            }
            let value = rest
                .next()
                .ok_or_else(|| format!("`{flag}` needs a value\n\n{USAGE}"))?;
            match flag.as_str() {
                "--command" => {
                    inputs.words = value.split_whitespace().map(str::to_owned).collect();
                }
                "--profile" => {
                    let profile = value.trim();
                    if !profile.is_empty() {
                        inputs.profile = profile.to_owned();
                    }
                }
                "--count" => inputs.count = number("--count", value, 10_000)?,
                "--load" => inputs.load = number("--load", value, 64)?,
                "--cores" => inputs.cores = number("--cores", value, 64)?,
                "--filter" => {
                    for expression in value.split(';').map(str::trim) {
                        if expression.is_empty() {
                            continue;
                        }
                        if !filter_text(expression) {
                            return Err(format!(
                                "`{expression}` has a character a filter does not pass on: \
                                 letters, digits, spaces and _-:./=,()|&!~*+@#^[]{{}}?\\ only"
                            ));
                        }
                        inputs.filters.push(expression.to_owned());
                    }
                }
                "--against" => {
                    let against = value.trim();
                    if !against.is_empty() {
                        if !plain(against) {
                            return Err(format!("`--against {against}` is not a ref"));
                        }
                        inputs.against = Some(against.to_owned());
                    }
                }
                other => return Err(format!("unknown flag `{other}`\n\n{USAGE}")),
            }
        }
        if !["ci", "release", "dist"].contains(&inputs.profile.as_str()) {
            return Err(format!(
                "`--profile {}`: ci, release or dist",
                inputs.profile
            ));
        }
        if let Some(word) = inputs.words.iter().find(|word| !plain(word)) {
            return Err(format!(
                "`{word}` has a character a try run does not pass on: letters, digits and \
                 _-:./=,()|&!~*+@# only, with words separated by spaces"
            ));
        }
        Ok(inputs)
    }

    /// The `cargo xtask` arguments the inputs come to.
    fn command(&self) -> Result<Vec<String>> {
        let Some((verb, rest)) = self.words.split_first() else {
            return Err(format!("say what to run\n\n{USAGE}"));
        };
        if !COMMANDS.contains(&verb.as_str()) {
            return Err(format!(
                "`{verb}` is not a command a try run runs\n\n{USAGE}"
            ));
        }
        if !self.filters.is_empty() && !matches!(verb.as_str(), "test" | "stress") {
            return Err("`--filter` chooses tests: it goes with `test` or `stress`".to_owned());
        }
        if let Some(against) = &self.against {
            return self.ab(verb, rest, against);
        }
        let squeezed = self.load.is_some() || self.cores.is_some();
        if squeezed && !(verb == "stress" || verb == "test" && self.count.is_some()) {
            return Err(
                "`--load` and `--cores` go with a stress run: `stress`, or `test` \
                        with a count"
                    .to_owned(),
            );
        }
        let owned = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        let mut command = Vec::new();
        match verb.as_str() {
            "test" | "stress" if verb == "stress" || self.count.is_some() => {
                command.push("stress".to_owned());
                if !rest.iter().any(|word| word == "--count") {
                    command.extend(owned(&["--count", &self.count.unwrap_or(100).to_string()]));
                }
                command.extend(owned(&["--cargo-profile", &self.profile]));
                command.extend(self.with_filters(rest.to_vec()));
            }
            "test" if self.profile == "ci" => {
                // The blocking job's build and run, and the tier report:
                // packages before `--`, the rest for nextest after it.
                command.extend(owned(&["ci", "--tiers", "--top", "20"]));
                let (packages, others) = packages_and_others(rest)?;
                for package in packages {
                    command.extend(["-p".to_owned(), package]);
                }
                let others = self.with_filters(others);
                if !others.is_empty() {
                    command.push("--".to_owned());
                    command.extend(others);
                }
            }
            "test" => {
                command.extend(owned(&[
                    "test",
                    "--cargo-profile",
                    &self.profile,
                    "--profile",
                    "ci",
                ]));
                command.extend(self.with_filters(rest.to_vec()));
            }
            "startup" => {
                if self.profile == "ci" {
                    return Err(
                        "start-up is measured on a release build: choose the release or \
                                the dist profile"
                            .to_owned(),
                    );
                }
                command.extend(owned(&["startup", "--profile", &self.profile]));
                if let Some(count) = self.count {
                    command.extend(owned(&["--runs", &count.to_string()]));
                }
                command.extend(rest.iter().cloned());
            }
            _ => {
                // `perf` and `ci` take their words as they are.
                command.push(verb.clone());
                command.extend(rest.iter().cloned());
            }
        }
        if let Some(load) = self.load {
            command.extend(owned(&["--load", &load.to_string()]));
        }
        if let Some(cores) = self.cores {
            command.extend(owned(&["--cores", &cores.to_string()]));
        }
        Ok(command)
    }
}

impl Inputs {
    /// `-E EXPR` for each filter.
    fn filter_arguments(&self) -> Vec<String> {
        self.filters
            .iter()
            .flat_map(|filter| ["-E".to_owned(), filter.clone()])
            .collect()
    }

    /// `words` for nextest, with `-E EXPR` for each filter where nextest
    /// reads it: before a `--` of the words' own, after which the words go
    /// to the test binary.
    fn with_filters(&self, mut words: Vec<String>) -> Vec<String> {
        let at = words
            .iter()
            .position(|word| word == "--")
            .unwrap_or(words.len());
        words.splice(at..at, self.filter_arguments());
        words
    }

    /// The A/B run: `ab` against the tree the workflow checked out.
    fn ab(&self, verb: &str, rest: &[String], against: &str) -> Result<Vec<String>> {
        if verb != "test" || self.profile != "ci" {
            return Err(
                "`--against` compares the blocking jobs' tests of two refs: it goes with \
                 `test`, in the ci profile"
                    .to_owned(),
            );
        }
        if self.load.is_some() || self.cores.is_some() {
            return Err("`--load` and `--cores` go with a stress run, not an A/B run".to_owned());
        }
        let mut command: Vec<String> = ["ab", "--base", AB_BASE, "--label", against, "--top", "20"]
            .map(str::to_owned)
            .to_vec();
        if let Some(count) = self.count {
            command.extend(["--count".to_owned(), count.max(2).to_string()]);
        }
        let (packages, others) = packages_and_others(rest)?;
        for package in packages {
            command.extend(["-p".to_owned(), package]);
        }
        let others = self.with_filters(others);
        if !others.is_empty() {
            command.push("--".to_owned());
            command.extend(others);
        }
        Ok(command)
    }
}

/// The crates `words` name, in any of cargo's spellings (`-p x`,
/// `--package x`, `-px`, `--package=x`), and the rest of the words, which
/// go to nextest.
fn packages_and_others(words: &[String]) -> Result<(Vec<String>, Vec<String>)> {
    let mut packages = Vec::new();
    let mut others = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if word == "-p" || word == "--package" {
            packages.push(words.next().ok_or("`-p` needs a crate")?.clone());
        } else if let Some(package) = word.strip_prefix("--package=") {
            packages.push(package.to_owned());
        } else if let Some(package) = word.strip_prefix("-p").filter(|_| !word.starts_with("--")) {
            packages.push(package.to_owned());
        } else {
            others.push(word.clone());
        }
    }
    Ok((packages, others))
}

/// `cargo xtask try …`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let command = Inputs::parse(arguments)?.command()?;
    println!("$ cargo xtask {}", command.join(" "));
    crate::steps::append_job_summary(&format!("### `cargo xtask {}`\n\n", command.join(" ")));
    let (first, rest) = command.split_first().ok_or("nothing to run")?;
    crate::run(first, rest)
}

#[cfg(test)]
mod tests {
    use super::Inputs;

    fn try_command(arguments: &[&str]) -> Result<String, String> {
        let arguments: Vec<String> = arguments.iter().map(|word| (*word).to_owned()).collect();
        Inputs::parse(&arguments)?
            .command()
            .map(|command| command.join(" "))
    }

    #[test]
    fn a_test_run_is_the_blocking_jobs_with_the_tier_report() {
        assert_eq!(
            try_command(&[
                "--command",
                "test -p cortex-tools shell::",
                "--profile",
                "",
                "--count",
                ""
            ]),
            Ok("ci --tiers --top 20 -p cortex-tools -- shell::".to_owned())
        );
        assert_eq!(
            try_command(&["--command", "test"]),
            Ok("ci --tiers --top 20".to_owned())
        );
        assert_eq!(
            try_command(&["--command", "test -p cortex-base", "--profile", "release"]),
            Ok("test --cargo-profile release --profile ci -p cortex-base".to_owned())
        );
    }

    #[test]
    fn a_count_makes_a_test_run_a_stress_run() {
        assert_eq!(
            try_command(&[
                "--command",
                "test -p cortex-tools shell::cmd",
                "--count",
                "100"
            ]),
            Ok("stress --count 100 --cargo-profile ci -p cortex-tools shell::cmd".to_owned())
        );
        assert_eq!(
            try_command(&[
                "--command",
                "stress -p cortex-testkit",
                "--count",
                "50",
                "--load",
                "6",
                "--cores",
                "2"
            ]),
            Ok(
                "stress --count 50 --cargo-profile ci -p cortex-testkit --load 6 --cores 2"
                    .to_owned()
            )
        );
        // A count in the command wins.
        assert_eq!(
            try_command(&[
                "--command",
                "stress -p cortex-agent --count 300",
                "--count",
                "5"
            ]),
            Ok("stress --cargo-profile ci -p cortex-agent --count 300".to_owned())
        );
    }

    #[test]
    fn filters_and_nextest_options_reach_nextest_as_they_are_meant() {
        // Several filter expressions, spaces and all, each one argument.
        let arguments: Vec<String> = [
            "--command",
            "test --package=cortex-tools -pcortex-agent --run-ignored only shell::",
            "--filter",
            "test(/shell::/) & !test(=slow) ; package(cortex-agent) ;",
        ]
        .map(str::to_owned)
        .to_vec();
        let command = Inputs::parse(&arguments).unwrap().command().unwrap();
        assert_eq!(
            command,
            [
                "ci",
                "--tiers",
                "--top",
                "20",
                "-p",
                "cortex-tools",
                "-p",
                "cortex-agent",
                "--",
                "--run-ignored",
                "only",
                "shell::",
                "-E",
                "test(/shell::/) & !test(=slow)",
                "-E",
                "package(cortex-agent)"
            ]
        );
        assert_eq!(
            try_command(&[
                "--command",
                "test -p cortex-tools",
                "--count",
                "20",
                "--filter",
                "test(~shell)"
            ]),
            Ok("stress --count 20 --cargo-profile ci -p cortex-tools -E test(~shell)".to_owned())
        );
        assert_eq!(
            try_command(&[
                "--command",
                "test",
                "--profile",
                "release",
                "--filter",
                "test(=a)"
            ]),
            Ok("test --cargo-profile release --profile ci -E test(=a)".to_owned())
        );
        // Before a `--` of the command's own, which sends what follows to the
        // test binary.
        assert_eq!(
            try_command(&[
                "--command",
                "stress -p x -- --exact",
                "--filter",
                "test(=a)"
            ]),
            Ok("stress --count 100 --cargo-profile ci -p x -E test(=a) -- --exact".to_owned())
        );
        assert_eq!(
            try_command(&[
                "--command",
                "test -p x shell:: -- --exact",
                "--filter",
                "test(=a)"
            ]),
            Ok("ci --tiers --top 20 -p x -- shell:: -E test(=a) -- --exact".to_owned())
        );
        for (arguments, expected) in [
            (
                &["--command", "test", "--filter", "test(x); rm -rf /'"][..],
                "does not pass on",
            ),
            (
                &["--command", "test", "--filter", "test($HOME)"],
                "does not pass on",
            ),
            (
                &["--command", "perf", "--filter", "test(x)"],
                "goes with `test` or `stress`",
            ),
        ] {
            let error = try_command(arguments).unwrap_err();
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
    }

    #[test]
    fn against_makes_a_test_run_an_ab_run_of_the_two_refs() {
        assert_eq!(
            try_command(&[
                "--command",
                "test -p cortex-core",
                "--against",
                "main",
                "--count",
                "6",
                "--filter",
                "test(/slow/)"
            ]),
            Ok("ab --base ../ab-base --label main --top 20 --count 6 -p cortex-core -- -E test(/slow/)"
                .to_owned())
        );
        assert_eq!(
            try_command(&["--command", "test", "--against", "origin/main"]),
            Ok("ab --base ../ab-base --label origin/main --top 20".to_owned())
        );
        for (arguments, expected) in [
            (
                &["--command", "stress -p x", "--against", "main"][..],
                "goes with `test`",
            ),
            (
                &[
                    "--command",
                    "test",
                    "--against",
                    "main",
                    "--profile",
                    "release",
                ],
                "in the ci profile",
            ),
            (&["--command", "test", "--against", "a b"], "not a ref"),
            (
                &["--command", "test", "--against", "main", "--load", "2"],
                "not an A/B run",
            ),
        ] {
            let error = try_command(arguments).unwrap_err();
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
    }

    #[test]
    fn startup_is_measured_on_a_release_build() {
        assert_eq!(
            try_command(&["--command", "startup", "--profile", "dist", "--count", "40"]),
            Ok("startup --profile dist --runs 40".to_owned())
        );
        let error = try_command(&["--command", "startup"]).unwrap_err();
        assert!(error.contains("release build"), "{error}");
    }

    #[test]
    fn perf_and_ci_take_their_words_as_they_are() {
        assert_eq!(
            try_command(&["--command", "perf --runs 5"]),
            Ok("perf --runs 5".to_owned())
        );
        assert_eq!(
            try_command(&["--command", "ci --tiers -p cortex-llm"]),
            Ok("ci --tiers -p cortex-llm".to_owned())
        );
    }

    #[test]
    fn what_a_try_run_does_not_run_is_refused() {
        for (arguments, expected) in [
            (
                &["--command", "mutate step-12"][..],
                "not a command a try run runs",
            ),
            (&["--command", ""], "say what to run"),
            (&["--command", "test; curl evil"], "has a character"),
            (&["--command", "test $(id)"], "has a character"),
            (&["--command", "test -p 'x'"], "has a character"),
            (
                &["--command", "test", "--profile", "dev"],
                "ci, release or dist",
            ),
            (&["--command", "test", "--count", "0"], "a number from 1"),
            (&["--command", "test", "--count", "lots"], "a number from 1"),
            (
                &["--command", "test", "--load", "4"],
                "go with a stress run",
            ),
            (
                &["--command", "startup", "--cores", "2", "--profile", "dist"],
                "go with a stress run",
            ),
            (&["--command"], "needs a value"),
            (&["--shell", "x"], "unknown flag"),
        ] {
            let error = try_command(arguments).unwrap_err();
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
    }
}
