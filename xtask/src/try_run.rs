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
                       [--count N] [--load N] [--cores K]

COMMAND is one of:
  test [-p CRATE]... [FILTER]...   the tests, as the blocking job builds and runs
                                   them, with the tier report; with --count,
                                   `stress` with that many repeats
  stress -p CRATE... [ARGS]...     the tests many times over (--count repeats)
  startup                          start-up of `cortex` built in the profile
                                   (release or dist), --count runs
  perf [ARGS]...                   the performance budgets (step 20)
  ci [ARGS]...                     any form of `cargo xtask ci`

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
                command.extend(rest.iter().cloned());
            }
            "test" if self.profile == "ci" => {
                // The blocking job's build and run, and the tier report:
                // packages before `--`, the rest for nextest after it.
                command.extend(owned(&["ci", "--tiers", "--top", "20"]));
                let mut others = Vec::new();
                let mut words = rest.iter();
                while let Some(word) = words.next() {
                    if word == "-p" || word == "--package" {
                        let package = words.next().ok_or("`-p` needs a crate")?;
                        command.extend(["-p".to_owned(), package.clone()]);
                    } else {
                        others.push(word.clone());
                    }
                }
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
                command.extend(rest.iter().cloned());
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
