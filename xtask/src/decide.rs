//! `cargo xtask decide`: ask the permission engine about one command line
//! (`docs/testing-and-ci.md`, "Asking the permission engine").
//!
//! The engine is in `cortex-policy`, which `xtask` does not build: `xtask`
//! is built before every CI job, and the engine would add its crates to
//! each. So this runs the probe that is an ignored test of `cortex-policy`'s
//! integration binary, with the case in its environment, and shows what it
//! prints.

use std::process::Command;

use crate::Result;
use crate::workspace;

const USAGE: &str = "\
usage: cargo xtask decide [--level LEVEL] [--rule RULE]... [--shell posix|powershell|cmd]
                          [--workspace PATH] [--cwd PATH] [--untrusted] [--] COMMAND...

Prints what the permission engine decides of a `shell` call running COMMAND,
with its reasons and the analysis it stands on. The engine is the one the
command table's tests use: workspace /work/app, home /users/dev, a trusted
project, an enforcing sandbox, level supervised, no rules, POSIX syntax,
unless told otherwise.

  --level     read-only, supervised, guided, autonomous or unrestricted
  --rule      `user|project allow|ask|deny read|write|exec|network|ext PATTERN`,
              for example --rule 'user allow exec cargo build *'
  --untrusted the project is not trusted

  cargo xtask decide 'git status && git log'
  cargo xtask decide --rule 'project deny exec git push *' -- git push origin main";

/// The case, as the probe's environment.
fn environment(arguments: &[String]) -> Result<Vec<(&'static str, String)>> {
    let mut variables: Vec<(&'static str, String)> = Vec::new();
    let mut rules = Vec::new();
    let mut command = Vec::new();
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let mut value = |name: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("`{name}` needs a value\n\n{USAGE}"))
        };
        match argument.as_str() {
            "--" => {
                command.extend(rest.by_ref().cloned());
                break;
            }
            "--level" => variables.push(("CORTEX_DECIDE_LEVEL", value(argument)?)),
            "--rule" => rules.push(value(argument)?),
            "--shell" => variables.push(("CORTEX_DECIDE_SHELL", value(argument)?)),
            "--workspace" => variables.push(("CORTEX_DECIDE_WORKSPACE", value(argument)?)),
            "--cwd" => variables.push(("CORTEX_DECIDE_CWD", value(argument)?)),
            "--untrusted" => variables.push(("CORTEX_DECIDE_UNTRUSTED", "1".to_owned())),
            "--help" | "-h" => return Err(USAGE.to_owned()),
            flag if flag.starts_with('-') && command.is_empty() => {
                return Err(format!("unknown flag `{flag}`\n\n{USAGE}"));
            }
            word => command.push(word.to_owned()),
        }
    }
    if command.is_empty() {
        return Err(format!("which command?\n\n{USAGE}"));
    }
    if rules.iter().any(|rule| rule.contains('\n')) {
        return Err("a rule is one line".to_owned());
    }
    variables.push(("CORTEX_DECIDE_COMMAND", command.join(" ")));
    if !rules.is_empty() {
        variables.push(("CORTEX_DECIDE_RULES", rules.join("\n")));
    }
    Ok(variables)
}

/// `cargo xtask decide …`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let variables = environment(arguments)?;
    let output = Command::new(workspace::cargo())
        .args([
            "test",
            "--quiet",
            "--locked",
            "-p",
            "cortex-policy",
            "--test",
            "it",
            "--",
            "--ignored",
            "--exact",
            "decide::probe",
            "--nocapture",
        ])
        .envs(
            variables
                .iter()
                .map(|(name, value)| (*name, value.as_str())),
        )
        .current_dir(workspace::root())
        .output()
        .map_err(|error| format!("could not run cargo: {error}"))?;
    let printed = String::from_utf8_lossy(&output.stdout);
    // Only the probe's own lines: the test harness's are noise here.
    let decision: String = printed
        .lines()
        .skip_while(|line| !line.starts_with("command "))
        .take_while(|line| !(line.starts_with("test ") || line.trim() == "." || line.is_empty()))
        .map(|line| format!("{line}\n"))
        .collect();
    if output.status.success() && !decision.is_empty() {
        print!("{decision}");
        return Ok(());
    }
    let errors = String::from_utf8_lossy(&output.stderr);
    let why = errors
        .lines()
        .skip_while(|line| !line.contains("panicked at"))
        .nth(1)
        .unwrap_or("see cargo's output");
    Err(format!(
        "the probe failed: {why}\n{}",
        errors
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::environment;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn the_case_goes_to_the_probe_in_its_environment() {
        let variables = environment(&strings(&[
            "--level",
            "read-only",
            "--rule",
            "user allow exec cargo build *",
            "--rule",
            "project deny exec git push *",
            "--untrusted",
            "--",
            "git",
            "push",
            "--force",
        ]))
        .unwrap();
        let value = |name: &str| {
            variables
                .iter()
                .find(|(each, _)| *each == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value("CORTEX_DECIDE_COMMAND"), Some("git push --force"));
        assert_eq!(value("CORTEX_DECIDE_LEVEL"), Some("read-only"));
        assert_eq!(
            value("CORTEX_DECIDE_RULES"),
            Some("user allow exec cargo build *\nproject deny exec git push *")
        );
        assert_eq!(value("CORTEX_DECIDE_UNTRUSTED"), Some("1"));
        assert_eq!(value("CORTEX_DECIDE_SHELL"), None);

        // One word holding the whole line is the same line.
        let one = environment(&strings(&["git status && git log"])).unwrap();
        assert!(one.contains(&("CORTEX_DECIDE_COMMAND", "git status && git log".to_owned())));
    }

    #[test]
    fn a_case_that_makes_no_sense_is_refused() {
        for (words, expected) in [
            (&[][..], "which command?"),
            (&["--level"], "needs a value"),
            (&["--fast", "ls"], "unknown flag"),
            (&["--rule", "user allow exec a\nb", "ls"], "one line"),
        ] {
            let error = environment(&strings(words)).unwrap_err();
            assert!(error.contains(expected), "{words:?}: {error}");
        }
    }
}
